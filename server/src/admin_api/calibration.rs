//! 人工校准面（总规格 §79/§83 内容生产流水线的第一环：**人工校准 → 仿真试跑 → 世界质量回归**）。
//!
//! 本模块做**三维**（§79 流水线里「人工校准」明列的那三项）：
//! **阶段切分**（`world_templates.saga_id` / `stage_no`，迁移 0024）、
//! **身份池**（`skeleton_json.identityPool` → `assembled_json./assembly/identityAssignments`）、
//! **境界档**（`skeleton_json.realmTier` → `assembled_json./assembly/realmTier`，总规格 §6）。
//!
//! 三维彼此不是同构的，别混着读：阶段切分是**坐标**（连续性诊断），身份池是**各不相同的站位**
//! （分布诊断），境界档是**全员统一的一件戏服**（有没有 / 各阶是否在换 / 实例钉住没有）。
//!
//! # 🔴 只读，且只可视化、不可编辑
//!
//! 本模块的端点**全是** SELECT 聚合，无任何写入、无任何副作用，因此**不挂运营开关**（同 `dashboards`
//! 的处理：VALIDATION §0.1 约束的是「让用户看到 / 用到新能力」的写入面）。响应恒带
//! `editable:false` + `editPath`：校准参数的唯一写入路径仍是**建模板**
//! （`POST /admin/world-templates` 的 `sagaId` / `stageNo` / `skeletonJson.identityPool` /
//! `skeletonJson.realmTier`）。谁读到这份 JSON 都不该以为「这里能调」。
//!
//! # 🔴 身份池的真实效力（本模块必须如实呈现，不得让运营以为「调了就会变强」）
//!
//! | 层 | 状态（VALIDATION §0.3 七档） | 事实 |
//! |---|---|---|
//! | 分配层 | **Implemented** | `assembly::assign_identities`（内核匹配 + `DOMAIN_IDENTITY` 种子），结果钉进 `assembled_json` |
//! | 叙事感知层 | **Implemented** | `runtime::load_identity_display_names` 读回 → 他人 brief `唐三（户部主事）` + 本人 `self_identities`，进引擎 prompt 上下文 |
//! | 数值层 | **设计上永不生效** | §0.1 平权红线：身份不改判定 / 不改发奖 / 不开权限 / 不调难度 / 不改准入。戏份靠玩出来 |
//! | 校准闭环 | **Implemented**（2026-07-27） | `slo::calibration` 的身份维读数：按身份 id 分组的「相对均分倍率」（均值 / 中位数 / 零分观察数 + 各身份之间的集中度基尼），落在 `/admin/metrics/overview` 的 `narrativeSlo.calibration`。🔴 **只读，绝不回灌引擎** |
//!
//! 所以本页能回答的是「**分配结果长什么样、是否失衡**」；「这样分配之后戏份分布如何」现在有读数了
//! （见上表最后一行），但**读数建成 ≠ 闭环已验证**——闭环成立要等运营真的用它调过参、
//! 并在下一批世界上看到因果。本页仍然不下「配得对不对」的判语。
//!
//! # 🔴 境界档的真实效力：叙事层已接通，校准闭环仍是空的
//!
//! | 层 | 状态（§0.3 七档） | 事实 |
//! |---|---|---|
//! | 声明层 | **Implemented** | `assembly::RealmTier` schema + `validate_skeleton_refs` 第 6 段取值域校验 |
//! | 钉住层 | **Implemented** | 装配时原样钉进 `assembled_json./assembly/realmTier`（零抽样、不占 RNG 域） |
//! | 叙事感知层 | **Integrated** | `runtime::parse_realm_costume` 读回 `briefing` + `flavorNotes` → `RoundInput.realm_costume` → 引擎 `call_director` 的入场导演设局 prompt（§6「入场导演统一设定」）。**只有这两个字段进模型上下文** |
//! | 数值层 | **设计上永不生效** | §6「跨体系靠风味翻译，不靠数值换算」+ §0.1 平权：`RealmTier` 结构里一个数字都没有，有测试锁；接进叙事层后同样只改描写，不进任何判定域（`realm_tier_reaches_only_the_director_prompt`） |
//! | 校准闭环 | **Implemented**（2026-07-27） | `slo::calibration` 的戏服维读数：按钉住的境界档分桶的世界质量三指标（完读率 / 阻断率 / 结局分布）。🔴 **跨世界对比**——境界档全员统一，没有组内分布可看 |
//!
//! 所以境界档这一维现在能回答「**配了没有、各阶是不是在换、实例钉住没有、玩家那一篇会不会
//! 被这样描写**」，外加「**穿这件戏服的那批世界，质量三指标长什么样**」。
//! 但**读数建成 ≠ 闭环已验证**：没有运营真的据此调过档、也没有下一批世界佐证因果，
//! 所以这一维**仍然不下**「这件戏服配得对不对」的判语。前端 `EffectPanel` 必须把这五层原样渲染。
//!
//! # 双库可移植 SQL（db.rs 约定）
//!
//! 只用 `COUNT` / `SUM(CASE …)` / `IN` / `<>` / BIGINT 比较；`SUM` 一律 `CAST(… AS BIGINT)`
//! （PG 下 `SUM(bigint)` 返回 numeric，不 CAST 解码会炸）。无 JSONB、无 serial、无 `NOW()`、
//! 无 `strftime` / `date_trunc`。**JSON 一律在 Rust 侧解析**——`json_extract` 是 SQLite 方言。
//!
//! # 内存纪律
//!
//! 需要读 `skeleton_json`（可达数十 KB/份）的两处扫描一律**按主键分页**（`id > ? ORDER BY id ASC`），
//! 每页 `SCAN_PAGE` 行、解析完即丢，只留下几个整数——`fetch_all` 会把整个结果集一次性物化，
//! 拿它扫全表骨架会把后台一次刷新变成几十 MB 的峰值。

use std::collections::{BTreeMap, BTreeSet};

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{AnyPool, Row};

use crate::db::Placeholders;
use crate::app::AppState;
use crate::auth::AdminUser;
use crate::error::ApiError;

use super::require_role;

// ---------------- 扫描上限（宁可明说截断，也不把后台拖垮；口径同 slo 的 scan_row_cap） ----------------

/// 阶段总览一次扫描的模板行上限。超出即**丢掉末尾那个可能被切断的系列**并置 `truncated`——
/// 半个系列的连续性诊断（缺号 / 重号）是错的，报出去比不报更糟。
const SAGA_SCAN_MAX: usize = 2000;
/// 单个 Saga 详情最多展开的模板数（阶段数由剧情结构决定，量级十几个；上限只防脏数据）。
const SAGA_DETAIL_TEMPLATE_MAX: i64 = 200;
/// 缺号清单最多列几个（只有 999 号一个阶段时缺号有 998 个，列表本身就成了噪声）。
const MISSING_STAGES_MAX: usize = 50;
/// 骨架维度目录（身份池 / 境界档）一次扫描的模板行上限。
const TEMPLATE_DIRECTORY_SCAN_MAX: usize = 2000;
/// 需要读骨架的分页扫描每页行数（峰值内存 ≈ 本页骨架大小之和）。
const SCAN_PAGE: i64 = 200;
/// 实例侧扫描（身份分配分布 / 境界档钉住情况）默认 / 最大扫描世界数。
const WORLD_SCAN_DEFAULT: i64 = 100;
const WORLD_SCAN_MAX: i64 = 500;
/// 单条 `IN (…)` 最多绑几个参数（SQLite 老版本 `SQLITE_MAX_VARIABLE_NUMBER` 默认 999，
/// 不分批会在大页面上直接报错）。
const BIND_CHUNK: usize = 200;

/// 「算作在运行」的世界状态（与 `/admin/worlds` 监控页的运行中口径同源）。
const LIVE_WORLD_STATUSES: &[&str] = &["open", "running"];

/// `LIVE_WORLD_STATUSES` 的 SQL 字面量片段。取值是模块内常量（不含引号、非用户输入），
/// 故直接拼进 SQL 无注入面；用绑定参数反而要按状态数动态生成占位符，更容易写错。
fn live_status_sql() -> String {
    LIVE_WORLD_STATUSES.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(",")
}

// ============================================================================
// 维度一：阶段切分（saga_id / stage_no，迁移 0024）
// ============================================================================

/// 阶段总览用的模板投影（**刻意不含 skeleton**：总览可能扫到两千行，带骨架会把一次刷新变成几十 MB）。
struct SagaTplRow {
    saga_id: String,
    stage_no: i64,
    moderation: String,
    star_rating: i64,
    room_type: String,
    created_at: i64,
}

/// 连续性诊断：给定一个系列的全部 `stage_no`，算出 `(缺号, 重号, 未编号数)`。
///
/// 缺号口径是 `1..=max` 内没有模板的号——**从 1 起而不是从 min 起**：一个 3-4-5 的系列
/// 缺的正是开篇 1、2，从 min 起算会把最该被发现的「没有第一阶段」洗掉。
fn stage_continuity(stage_nos: &[i64]) -> (Vec<i64>, Vec<i64>, i64) {
    let mut seen: BTreeMap<i64, i64> = BTreeMap::new();
    let mut unnumbered = 0i64;
    for n in stage_nos {
        if *n <= 0 {
            // saga_id 非空却没有阶段号：建模板端点拦得住，直写库 / 历史数据拦不住 → 明确报出来。
            unnumbered += 1;
            continue;
        }
        *seen.entry(*n).or_insert(0) += 1;
    }
    let duplicates: Vec<i64> = seen.iter().filter(|(_, c)| **c > 1).map(|(n, _)| *n).collect();
    let max = seen.keys().next_back().copied().unwrap_or(0);
    let mut missing: Vec<i64> = Vec::new();
    for n in 1..=max {
        if !seen.contains_key(&n) {
            missing.push(n);
            if missing.len() >= MISSING_STAGES_MAX {
                break;
            }
        }
    }
    (missing, duplicates, unnumbered)
}

/// 每个 Saga 的世界实例计数 `(总数, 运行中)`：`world_templates ⋈ worlds` 一次 GROUP BY 取全表。
/// 走 JOIN 而不是「先取模板 id 再 `IN (…)`」——总览的模板 id 可能上千，`IN` 要分批且更慢。
async fn saga_world_counts(db: &AnyPool) -> Result<BTreeMap<String, (i64, i64)>, ApiError> {
    let live = live_status_sql();
    let sql = format!(
        "SELECT t.saga_id AS saga_id, COUNT(w.id) AS n, \
         CAST(COALESCE(SUM(CASE WHEN w.status IN ({live}) THEN 1 ELSE 0 END), 0) AS BIGINT) AS live_n \
         FROM world_templates t JOIN worlds w ON w.template_id = t.id \
         WHERE t.saga_id <> '' GROUP BY t.saga_id"
    );
    let mut out = BTreeMap::new();
    for r in sqlx::query(&sql).fetch_all(db).await? {
        out.insert(
            r.try_get::<String, _>("saga_id")?,
            (r.try_get::<i64, _>("n")?, r.try_get::<i64, _>("live_n")?),
        );
    }
    Ok(out)
}

/// GET /admin/sagas：**阶段切分总览**。
///
/// 一屏回答「平台上有哪些世界系列、各被切成几个阶段、切分是否成形」：每个系列给出阶段数、
/// 阶段号范围、**缺号 / 重号 / 未编号**三项连续性诊断、审核态分布、星级跨度，以及由这些阶段
/// 开出的世界实例数。
///
/// 另给 `standaloneTemplateCount`（`saga_id = ''` 的模板数）作对照——那是**尚未被切分**的存量，
/// 校准工作的分母。
pub(super) async fn list_sagas(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;

    let rows = sqlx::query(
        "SELECT saga_id, stage_no, moderation, star_rating, room_type, created_at \
         FROM world_templates WHERE saga_id <> '' \
         ORDER BY saga_id ASC, stage_no ASC, id ASC LIMIT $1",
    )
    .bind(SAGA_SCAN_MAX as i64 + 1)
    .fetch_all(&state.db)
    .await?;

    let truncated = rows.len() > SAGA_SCAN_MAX;
    let mut scanned: Vec<SagaTplRow> = Vec::with_capacity(rows.len().min(SAGA_SCAN_MAX));
    for r in rows.iter().take(SAGA_SCAN_MAX) {
        scanned.push(SagaTplRow {
            saga_id: r.try_get("saga_id")?,
            stage_no: r.try_get("stage_no")?,
            moderation: r.try_get("moderation")?,
            star_rating: r.try_get("star_rating")?,
            room_type: r.try_get("room_type")?,
            created_at: r.try_get("created_at")?,
        });
    }
    // 截断时末尾那个系列多半只取到一半，其连续性诊断必然是错的 → 整组丢弃
    //（`ORDER BY saga_id` 保证同一系列的行相邻，故只需丢最后一个 saga_id）。
    if truncated {
        if let Some(last) = scanned.last().map(|r| r.saga_id.clone()) {
            scanned.retain(|r| r.saga_id != last);
        }
    }

    let world_counts = saga_world_counts(&state.db).await?;

    let mut grouped: BTreeMap<String, Vec<&SagaTplRow>> = BTreeMap::new();
    for r in &scanned {
        grouped.entry(r.saga_id.clone()).or_default().push(r);
    }

    let mut sagas = Vec::with_capacity(grouped.len());
    for (saga_id, items) in &grouped {
        let stage_nos: Vec<i64> = items.iter().map(|r| r.stage_no).collect();
        let (missing, duplicates, unnumbered) = stage_continuity(&stage_nos);
        let distinct: BTreeSet<i64> = stage_nos.iter().copied().filter(|n| *n > 0).collect();
        let mut moderation: BTreeMap<&str, i64> = BTreeMap::new();
        let mut room_types: BTreeSet<&str> = BTreeSet::new();
        let (mut star_min, mut star_max) = (i64::MAX, i64::MIN);
        let (mut first_created, mut last_created) = (i64::MAX, i64::MIN);
        for r in items {
            *moderation.entry(r.moderation.as_str()).or_insert(0) += 1;
            room_types.insert(r.room_type.as_str());
            star_min = star_min.min(r.star_rating);
            star_max = star_max.max(r.star_rating);
            first_created = first_created.min(r.created_at);
            last_created = last_created.max(r.created_at);
        }
        let (world_count, live_world_count) = world_counts.get(saga_id).copied().unwrap_or((0, 0));
        let missing_truncated = missing.len() >= MISSING_STAGES_MAX;
        sagas.push(json!({
            "sagaId": saga_id,
            "templateCount": items.len() as i64,
            "stageCount": distinct.len() as i64,
            "minStageNo": distinct.iter().next().copied(),
            "maxStageNo": distinct.iter().next_back().copied(),
            // 连续性三诊断：切分是否成形，全看这三项。
            "missingStageNos": missing,
            "missingStageNosTruncated": missing_truncated,
            "duplicateStageNos": duplicates,
            "unnumberedTemplateCount": unnumbered,
            "contiguous": !missing_truncated && missing.is_empty() && duplicates.is_empty() && unnumbered == 0,
            "moderationCounts": moderation,
            "roomTypes": room_types,
            "starMin": (star_min != i64::MAX).then_some(star_min),
            "starMax": (star_max != i64::MIN).then_some(star_max),
            "worldCount": world_count,
            "liveWorldCount": live_world_count,
            "firstCreatedAt": (first_created != i64::MAX).then_some(first_created),
            "lastCreatedAt": (last_created != i64::MIN).then_some(last_created),
        }));
    }

    let standalone: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM world_templates WHERE saga_id = ''")
            .fetch_one(&state.db)
            .await?
            .try_get("n")?;

    Ok(Json(json!({
        "sagas": sagas,
        "scannedTemplates": scanned.len() as i64,
        "truncated": truncated,
        "standaloneTemplateCount": standalone,
        "editable": false,
        "editPath": "阶段坐标只在建模板时录入：POST /admin/world-templates 的 sagaId + stageNo（二者必须成对，stageNo ∈ 1..=999）。本端点只读，不提供改号 / 移阶段 / 重排。",
        "notes": [
            "缺号口径为 1..=maxStageNo 内没有模板的阶段号（从 1 起算，故「缺开篇」也会被报出来）。",
            "contiguous = 无缺号 ∧ 无重号 ∧ 无未编号；它只说明阶段坐标齐整，不代表内容质量合格。",
            "truncated=true 时，扫描末尾那个可能被切断的系列已整组丢弃（半个系列的连续性诊断是错的）。",
            "worldCount 统计由该系列各阶段模板开出的全部世界实例（不限可见性、不限状态）。",
        ],
    })))
}

// ---------------- 单个 Saga：逐阶段结构 ----------------

/// 骨架形状指标：一个阶段"什么形状"，全看这几个数。
///
/// 全部在 Rust 侧解析（`json_extract` 是 SQLite 方言，双库禁用）；解析失败 → `parsed:false`
/// 且各项**字段缺席**，绝不编 0——「骨架坏了」与「骨架里真的一个主线节点都没有」是两件事。
fn skeleton_shape(raw: &str) -> Value {
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return json!({ "parsed": false, "skeletonBytes": raw.len() as i64 });
    };
    let arr_len = |key: &str| -> i64 {
        v.get(key).and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0)
    };
    let pool = parse_identity_pool(raw);
    let realm = parse_realm_tier(raw);
    json!({
        "parsed": true,
        "skeletonBytes": raw.len() as i64,
        "mainlineNodes": arr_len("mainlineNodes"),
        "endingPool": arr_len("endingPool"),
        "hiddenContentPool": arr_len("hiddenContentPool"),
        "worldCharacters": arr_len("worldCharacters"),
        "locations": arr_len("locations"),
        "storylines": arr_len("storylines"),
        "subplotCardRefs": arr_len("subplotCardRefs"),
        "seams": arr_len("seams"),
        "identityPoolSize": pool.len() as i64,
        "identityQuotaTotal": pool.iter().map(|s| s.quota.max(0)).sum::<i64>(),
        "identityLeadCount": pool.iter().filter(|s| s.is_lead).count() as i64,
        "hasPayoutTable": v.get("payoutTable").is_some_and(|x| !x.is_null()),
        // 境界档是「有 / 无」的一件戏服，不是可计数的池 —— 故这里只给布尔 + 档名，
        // 和 identityPoolSize 那种规模指标不同构（§6 全员统一，没有"几个境界"这回事）。
        "hasRealmTier": realm.is_some(),
        "realmTierLabel": realm.as_ref().map(RealmEntry::display),
        "hasEndgame": v.get("endgame").is_some_and(|x| !x.is_null()),
        "isSuperset": v.get("isSuperset").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// GET /admin/sagas/{sagaId}：**单个世界系列的逐阶段结构**。
///
/// 阶段按 `stage_no` 升序（剧情顺序，不是录入时间，口径同 `GET /admin/world-templates?sagaId=`）；
/// 每阶段给出模板元数据 + 骨架形状指标 + 该阶段已开出的世界实例数。
/// 系列不存在（无任何模板挂在该 `saga_id` 下）→ 404。
pub(super) async fn saga_detail(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(saga_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let saga_id = saga_id.trim().to_string();
    if saga_id.is_empty() {
        return Err(ApiError::BadRequest("sagaId 不能为空".into()));
    }

    let rows = sqlx::query(
        "SELECT id, title, room_type, moderation, star_rating, star_source, official, version, \
         stage_no, created_at, skeleton_json \
         FROM world_templates WHERE saga_id = $1 ORDER BY stage_no ASC, id ASC LIMIT $2",
    )
    .bind(&saga_id)
    .bind(SAGA_DETAIL_TEMPLATE_MAX)
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        return Err(ApiError::NotFound);
    }

    let mut ids: Vec<String> = Vec::with_capacity(rows.len());
    for r in &rows {
        ids.push(r.try_get::<String, _>("id")?);
    }
    let counts = world_counts_by_template(&state.db, &ids).await?;

    // 逐阶段归组：同一 stage_no 下可能有多个模板（重号；本身就是要被看见的校准问题）。
    let mut stages: Vec<Value> = Vec::new();
    let mut stage_nos: Vec<i64> = Vec::new();
    let mut cur_stage: Option<i64> = None;
    let mut cur_items: Vec<Value> = Vec::new();
    for r in &rows {
        let stage_no: i64 = r.try_get("stage_no")?;
        stage_nos.push(stage_no);
        if cur_stage != Some(stage_no) {
            if let Some(prev) = cur_stage {
                stages.push(json!({ "stageNo": prev, "templates": std::mem::take(&mut cur_items) }));
            }
            cur_stage = Some(stage_no);
        }
        let id: String = r.try_get("id")?;
        let skeleton_raw: String = r.try_get("skeleton_json")?;
        let (world_count, live_world_count) = counts.get(&id).copied().unwrap_or((0, 0));
        cur_items.push(json!({
            "id": id,
            "title": r.try_get::<String, _>("title")?,
            "roomType": r.try_get::<String, _>("room_type")?,
            "moderation": r.try_get::<String, _>("moderation")?,
            "starRating": r.try_get::<i64, _>("star_rating")?,
            "starSource": r.try_get::<String, _>("star_source")?,
            "official": r.try_get::<i64, _>("official")? != 0,
            "version": r.try_get::<i64, _>("version")?,
            "createdAt": r.try_get::<i64, _>("created_at")?,
            "worldCount": world_count,
            "liveWorldCount": live_world_count,
            "shape": skeleton_shape(&skeleton_raw),
        }));
    }
    if let Some(prev) = cur_stage {
        stages.push(json!({ "stageNo": prev, "templates": cur_items }));
    }

    let (missing, duplicates, unnumbered) = stage_continuity(&stage_nos);
    let distinct: BTreeSet<i64> = stage_nos.iter().copied().filter(|n| *n > 0).collect();
    let missing_truncated = missing.len() >= MISSING_STAGES_MAX;

    Ok(Json(json!({
        "sagaId": saga_id,
        "templateCount": rows.len() as i64,
        "stageCount": distinct.len() as i64,
        "stages": stages,
        "continuity": {
            "minStageNo": distinct.iter().next().copied(),
            "maxStageNo": distinct.iter().next_back().copied(),
            "missingStageNos": missing,
            "missingStageNosTruncated": missing_truncated,
            "duplicateStageNos": duplicates,
            "unnumberedTemplateCount": unnumbered,
            "contiguous": !missing_truncated && missing.is_empty() && duplicates.is_empty() && unnumbered == 0,
        },
        "truncated": rows.len() as i64 >= SAGA_DETAIL_TEMPLATE_MAX,
        "editable": false,
        "editPath": "阶段坐标与骨架只在建模板时录入：POST /admin/world-templates。本端点只读。",
        "notes": [
            "阶段按 stage_no 升序 = 剧情顺序，不是录入时间。",
            "shape.parsed=false 表示该模板 skeleton_json 不是合法 JSON，各形状指标缺席（不是 0）。",
            "shape 只描述骨架规模，不评价内容质量；质量走仿真试跑与世界质量回归（slo/quality.rs）。",
            "identityQuotaTotal 是配额上限之和，不是保底：实际站几个人由实例种子与在场人数共同决定。",
        ],
    })))
}

/// 逐模板的世界实例计数 `(总数, 运行中)`，按 `IN (…)` 分批取（避开 SQLite 变量数上限）。
async fn world_counts_by_template(
    db: &AnyPool,
    ids: &[String],
) -> Result<BTreeMap<String, (i64, i64)>, ApiError> {
    let mut out = BTreeMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let live = live_status_sql();
    for chunk in ids.chunks(BIND_CHUNK) {
        // `{live}` 是内联的状态字面量（非参数），故本条只有 ids 这一串参数，从 $1 起发号。
        let placeholders = Placeholders::new().list(chunk.len());
        let sql = format!(
            "SELECT template_id, COUNT(*) AS n, \
             CAST(COALESCE(SUM(CASE WHEN status IN ({live}) THEN 1 ELSE 0 END), 0) AS BIGINT) AS live_n \
             FROM worlds WHERE template_id IN ({placeholders}) GROUP BY template_id"
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id.as_str());
        }
        for r in q.fetch_all(db).await? {
            out.insert(
                r.try_get::<String, _>("template_id")?,
                (r.try_get::<i64, _>("n")?, r.try_get::<i64, _>("live_n")?),
            );
        }
    }
    Ok(out)
}

// ============================================================================
// 维度二：身份池（skeleton.identityPool → assembled_json.identityAssignments）
// ============================================================================

/// 身份池条目的**只读投影**。
///
/// 刻意不复用 `assembly::IdentitySpec`（私有、且带装配语义）：本模块只要「运营看得见的那几个字段」，
/// 且要对脏数据**宽容**（缺 id 的条目照样报出来供运营发现），而装配层对同样的脏数据是
/// 「拒绝建模板」。两种态度都对，但不该共用一个类型。
struct IdentityEntry {
    id: String,
    label: String,
    quota: i64,
    themes: Vec<String>,
    hook_affinity: Vec<String>,
    is_lead: bool,
}

/// 解析 `skeleton_json.identityPool[]`。缺省 / 结构不符 → 空（老模板零影响）。
/// `quota` 缺省 1（与 `assembly::IdentitySpec::quota` 的默认一致）。
fn parse_identity_pool(skeleton_raw: &str) -> Vec<IdentityEntry> {
    let Ok(v) = serde_json::from_str::<Value>(skeleton_raw) else {
        return Vec::new();
    };
    let Some(arr) = v.get("identityPool").and_then(Value::as_array) else {
        return Vec::new();
    };
    let str_list = |x: Option<&Value>| -> Vec<String> {
        x.and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    arr.iter()
        .map(|spec| IdentityEntry {
            id: spec.get("id").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            label: spec.get("label").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            quota: spec.get("quota").and_then(Value::as_i64).unwrap_or(1),
            themes: str_list(spec.get("themes")),
            hook_affinity: str_list(spec.get("hookAffinity")),
            is_lead: spec.get("isLead").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect()
}

/// 从实例 `assembled_json` 读回 `[(cid, identityId), …]`。
///
/// 结构口径与 `runtime::parse_identity_assignments` **逐字一致**（同一个 JSON 指针、同样的防御式
/// 跳过），但刻意不跨模块调用：那个函数是 runtime 私有的，且本模块是只读观测面——观测面挂了
/// 不该有能力影响 tick，两边各自防御式解析正是要的隔离。
fn parse_assignments(assembled_json: Option<&str>) -> Vec<(String, String)> {
    let Some(raw) = assembled_json else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(arr) = v.pointer("/assembly/identityAssignments").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pair in arr {
        let Some(p) = pair.as_array() else { continue };
        let (Some(cid), Some(iid)) = (
            p.first().and_then(Value::as_str).map(str::trim),
            p.get(1).and_then(Value::as_str).map(str::trim),
        ) else {
            continue;
        };
        if cid.is_empty() || iid.is_empty() {
            continue;
        }
        out.push((cid.to_string(), iid.to_string()));
    }
    out
}

/// GET /admin/identity-pools：**声明了身份池的模板目录**。
///
/// 校准的入口问题是「哪些模板配了身份池、池有多大」——没有这张表，运营只能逐个模板点开碰运气。
/// 按主键分页扫描（每页 `SCAN_PAGE` 行，解析完即丢），扫满 `TEMPLATE_DIRECTORY_SCAN_MAX` 行即
/// 停并置 `truncated`；**未声明 `identityPool` 的模板不进结果**（它们在这一维上零影响）。
pub(super) async fn list_identity_pools(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;

    let mut cursor = String::new();
    let mut scanned = 0usize;
    let mut items: Vec<Value> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let mut truncated = false;
    loop {
        let page = sqlx::query(
            "SELECT id, title, room_type, moderation, saga_id, stage_no, skeleton_json \
             FROM world_templates WHERE id > $1 ORDER BY id ASC LIMIT $2",
        )
        .bind(&cursor)
        .bind(SCAN_PAGE)
        .fetch_all(&state.db)
        .await?;
        if page.is_empty() {
            break;
        }
        for r in &page {
            let id: String = r.try_get("id")?;
            cursor = id.clone();
            scanned += 1;
            let raw: String = r.try_get("skeleton_json")?;
            let pool = parse_identity_pool(&raw);
            if pool.is_empty() {
                continue; // 未声明身份池 = 这一维上零影响，不占目录篇幅。
            }
            ids.push(id.clone());
            items.push(json!({
                "templateId": id,
                "title": r.try_get::<String, _>("title")?,
                "roomType": r.try_get::<String, _>("room_type")?,
                "moderation": r.try_get::<String, _>("moderation")?,
                "sagaId": r.try_get::<String, _>("saga_id")?,
                "stageNo": r.try_get::<i64, _>("stage_no")?,
                "poolSize": pool.len() as i64,
                "quotaTotal": pool.iter().map(|e| e.quota.max(0)).sum::<i64>(),
                "leadCount": pool.iter().filter(|e| e.is_lead).count() as i64,
            }));
        }
        if scanned >= TEMPLATE_DIRECTORY_SCAN_MAX {
            truncated = page.len() as i64 == SCAN_PAGE;
            break;
        }
    }

    // 世界实例数：让运营先看「这个池到底跑过没有」，再决定点进哪一个看分布。
    let counts = world_counts_by_template(&state.db, &ids).await?;
    for item in &mut items {
        let id = item["templateId"].as_str().unwrap_or_default().to_string();
        let (n, live) = counts.get(&id).copied().unwrap_or((0, 0));
        item["worldCount"] = json!(n);
        item["liveWorldCount"] = json!(live);
    }

    Ok(Json(json!({
        "templates": items,
        "scannedTemplates": scanned as i64,
        "truncated": truncated,
        "editable": false,
        "editPath": "身份池只在建模板时录入：POST /admin/world-templates 的 skeletonJson.identityPool。本端点只读。",
        "notes": [
            "只列出声明了非空 identityPool 的模板；其余模板在身份维上零影响，故不出现在本目录。",
            "worldCount=0 表示这个池从未被任何世界用过 —— 分布页对它只会显示空态，不是 0%。",
        ],
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorldScanQuery {
    /// 扫描多少个该模板开出的世界实例（按创建时间倒序取最近的）。默认 100，clamp [1, 500]。
    limit: Option<i64>,
}

/// 逐世界的 active 成员 cid，分批 `IN (…)` 取回。
async fn active_members_by_world(
    db: &AnyPool,
    world_ids: &[String],
) -> Result<BTreeMap<String, Vec<String>>, ApiError> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if world_ids.is_empty() {
        return Ok(out);
    }
    for chunk in world_ids.chunks(BIND_CHUNK) {
        // 整条语句只有这一串参数，故从 $1 起顺序发号，与下面 bind 的循环顺序一致。
        let placeholders = Placeholders::new().list(chunk.len());
        let sql = format!(
            "SELECT world_id, cloud_character_id FROM world_members \
             WHERE status = 'active' AND world_id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        for id in chunk {
            q = q.bind(id.as_str());
        }
        for r in q.fetch_all(db).await? {
            out.entry(r.try_get::<String, _>("world_id")?)
                .or_default()
                .push(r.try_get::<String, _>("cloud_character_id")?);
        }
    }
    Ok(out)
}

/// GET /admin/world-templates/{id}/identity-pool?limit=：**身份池声明 + 实际分配分布**。
///
/// 左边是模板声明的池（每个站位的配额 / 主题词 / 钩子引力 / 是否戏眼），右边是这个模板真的开出来的
/// 世界里**分配成了什么样**：每个身份被分到几人次、覆盖几个世界、相对配额的填充率、从没被分到过的
/// 站位、以及在场却没拿到站位的角色数。集中度用基尼（**复用 `slo::gini_coefficient`**，与叙事
/// 注意力基尼同一个实现，不另立第二套算法）。
///
/// 🔴 **只回答"分配成了什么样"，不回答"这样分配好不好"**：身份当前只进叙事称谓与角色自视上下文
/// （`runtime::load_identity_display_names`），按平权红线永不进数值层；且全仓没有任何指标度量
/// 「身份池调整 → 戏份分布变化」。响应的 `effect` 段把这四层状态原样下发，**前端必须显式渲染**，
/// 不得只画分布图。
pub(super) async fn template_identity_pool(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(template_id): Path<String>,
    Query(q): Query<WorldScanQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let scan = q.limit.unwrap_or(WORLD_SCAN_DEFAULT).clamp(1, WORLD_SCAN_MAX);

    let tpl = sqlx::query(
        "SELECT id, title, room_type, version, moderation, saga_id, stage_no, skeleton_json \
         FROM world_templates WHERE id = $1",
    )
    .bind(&template_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let skeleton_raw: String = tpl.try_get("skeleton_json")?;
    let pool = parse_identity_pool(&skeleton_raw);

    // 池内自检：重复 id / 空 id / 非正配额。建模板期 `validate_skeleton_refs` 会拦，但历史数据与
    // 直写库拦不住——校准面的价值之一就是把这些「本不该存在」的行摆到运营面前。
    let mut seen: BTreeMap<&str, i64> = BTreeMap::new();
    for e in &pool {
        *seen.entry(e.id.as_str()).or_insert(0) += 1;
    }
    let duplicate_ids: Vec<&str> =
        seen.iter().filter(|(id, c)| **c > 1 && !id.is_empty()).map(|(id, _)| *id).collect();
    let blank_id_count = pool.iter().filter(|e| e.id.is_empty()).count() as i64;
    let bad_quota_ids: Vec<&str> =
        pool.iter().filter(|e| e.quota <= 0 && !e.id.is_empty()).map(|e| e.id.as_str()).collect();

    // 该模板开出的世界（最近 scan 个）。
    let world_rows = sqlx::query(
        "SELECT id, title, status, assembled_json FROM worlds WHERE template_id = $1 \
         ORDER BY created_at DESC, id DESC LIMIT $2",
    )
    .bind(&template_id)
    .bind(scan + 1)
    .fetch_all(&state.db)
    .await?;
    let worlds_truncated = world_rows.len() as i64 > scan;
    let world_rows: Vec<_> = world_rows.into_iter().take(scan as usize).collect();

    let mut world_ids: Vec<String> = Vec::with_capacity(world_rows.len());
    for r in &world_rows {
        world_ids.push(r.try_get::<String, _>("id")?);
    }
    let members = active_members_by_world(&state.db, &world_ids).await?;

    // ---- 聚合 ----
    let mut assigned_count: BTreeMap<String, i64> = BTreeMap::new(); // identityId → 被分配人次
    let mut world_span: BTreeMap<String, BTreeSet<String>> = BTreeMap::new(); // identityId → 出现过的世界
    let mut worlds_assembled = 0i64;
    let mut worlds_with_assignments = 0i64;
    let mut assignment_total = 0i64;
    let mut active_member_total = 0i64;
    let mut members_without_identity = 0i64;
    let mut world_items: Vec<Value> = Vec::with_capacity(world_rows.len());

    for r in &world_rows {
        let wid: String = r.try_get("id")?;
        let assembled: Option<String> = r.try_get("assembled_json")?;
        let has_assembled = assembled.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        if has_assembled {
            worlds_assembled += 1;
        }
        let assignments = parse_assignments(assembled.as_deref());
        if !assignments.is_empty() {
            worlds_with_assignments += 1;
        }
        assignment_total += assignments.len() as i64;
        let assigned_cids: BTreeSet<&str> = assignments.iter().map(|(c, _)| c.as_str()).collect();
        for (_, iid) in &assignments {
            *assigned_count.entry(iid.clone()).or_insert(0) += 1;
            world_span.entry(iid.clone()).or_default().insert(wid.clone());
        }
        let world_members = members.get(&wid).map(Vec::as_slice).unwrap_or(&[]);
        let without =
            world_members.iter().filter(|c| !assigned_cids.contains(c.as_str())).count() as i64;
        active_member_total += world_members.len() as i64;
        members_without_identity += without;
        world_items.push(json!({
            "id": wid,
            "title": r.try_get::<String, _>("title")?,
            "status": r.try_get::<String, _>("status")?,
            "assembled": has_assembled,
            "assignmentCount": assignments.len() as i64,
            "activeMemberCount": world_members.len() as i64,
            "activeMembersWithoutIdentity": without,
        }));
    }

    // 逐身份：分配人次 / 覆盖世界数 / 相对配额填充率。
    // 填充率分母 = quota × **有分配的世界数**：没装配 / 没分配的世界根本没参与过这轮分配，
    // 把它们算进分母会把填充率无端稀释（同 `/admin/worlds` 成功率只算已终结 tick 的哲学）。
    let mut by_identity: Vec<Value> = Vec::with_capacity(pool.len());
    let mut counts_for_gini: Vec<i64> = Vec::with_capacity(pool.len());
    let mut never_assigned: Vec<&str> = Vec::new();
    for e in &pool {
        let n = assigned_count.get(&e.id).copied().unwrap_or(0);
        let capacity = e.quota.max(0) * worlds_with_assignments;
        let fill: Option<f64> = (capacity > 0).then(|| n as f64 / capacity as f64);
        if !e.id.is_empty() {
            counts_for_gini.push(n);
            if n == 0 && worlds_with_assignments > 0 {
                never_assigned.push(e.id.as_str());
            }
        }
        by_identity.push(json!({
            "identityId": e.id,
            "label": if e.label.is_empty() { e.id.clone() } else { e.label.clone() },
            "quota": e.quota,
            "isLead": e.is_lead,
            "themes": e.themes,
            "hookAffinity": e.hook_affinity,
            "assignedCount": n,
            "worldCount": world_span.get(&e.id).map(BTreeSet::len).unwrap_or(0) as i64,
            "quotaCapacity": capacity,
            // 0..1 小数（渲染须 ×100）；无有效分母 → null，显示 —，**不得当 0% 读**。
            "fillRatio": fill,
        }));
    }
    // 分配里出现、池里查不到的身份 id：模板改过版本、老实例还钉着旧身份 → 叙事层对这些角色
    // 退化为只显示名字（`load_identity_display_names` 查不到 label 即跳过）。
    let pool_ids: BTreeSet<&str> = pool.iter().map(|e| e.id.as_str()).collect();
    let unknown: Vec<Value> = assigned_count
        .iter()
        .filter(|(iid, _)| !pool_ids.contains(iid.as_str()))
        .map(|(iid, n)| json!({ "identityId": iid, "assignedCount": n }))
        .collect();

    // 集中度：**只对声明池内的身份**算，且只在「≥2 个身份 ∧ 真发生过分配」时给值；
    // 否则 null（"没开演"不是"很不均衡"，同 dashboards::ratio_or_null 的哲学）。
    let gini: Option<f64> = (counts_for_gini.len() >= 2 && assignment_total > 0)
        .then(|| crate::slo::gini_coefficient(&counts_for_gini));

    Ok(Json(json!({
        "templateId": tpl.try_get::<String, _>("id")?,
        "title": tpl.try_get::<String, _>("title")?,
        "roomType": tpl.try_get::<String, _>("room_type")?,
        "version": tpl.try_get::<i64, _>("version")?,
        "moderation": tpl.try_get::<String, _>("moderation")?,
        "sagaId": tpl.try_get::<String, _>("saga_id")?,
        "stageNo": tpl.try_get::<i64, _>("stage_no")?,
        "declared": !pool.is_empty(),
        "poolSize": pool.len() as i64,
        "quotaTotal": pool.iter().map(|e| e.quota.max(0)).sum::<i64>(),
        "leadCount": pool.iter().filter(|e| e.is_lead).count() as i64,
        // 池自检（本不该出现的脏数据；建模板端点会拦，历史 / 直写库不会）
        "poolIssues": {
            "duplicateIds": duplicate_ids,
            "blankIdCount": blank_id_count,
            "nonPositiveQuotaIds": bad_quota_ids,
        },
        "distribution": {
            "worldsScanned": world_rows.len() as i64,
            "worldsTruncated": worlds_truncated,
            "worldsAssembled": worlds_assembled,
            "worldsWithAssignments": worlds_with_assignments,
            "assignmentTotal": assignment_total,
            "activeMemberTotal": active_member_total,
            "activeMembersWithoutIdentity": members_without_identity,
            "byIdentity": by_identity,
            "unknownIdentityIds": unknown,
            "neverAssignedIdentityIds": never_assigned,
            "gini": gini,
            "worlds": world_items,
        },
        // 🔴 效力自述：前端必须显式渲染，不得只画分布图（VALIDATION §0.3 状态语言七档）。
        "effect": {
            "assignmentLayer": "Implemented",
            "narrativeLayer": "Implemented",
            "numericLayer": "NeverByDesign",
            // 2026-07-27：`slo::calibration` 的身份维读数上线后，本层从 Missing 转为 Implemented。
            // 🔴 七档天花板就在这里：读数只是**能测了**，不是「已验证配得对」——
            // 后者要等运营真的据此调过参并在下一批世界上看到因果（Validated）。谁想往上抬先补那条证据。
            "calibrationLoop": "Implemented",
            // 🔴 下发文案一律**纯文本**：前端把它当普通字符串渲染，写 Markdown 星号只会在界面上
            // 露出两个字面的 `**`（已在验收中出现过一次）。强调一律用中文引号「」。
            "summary": "身份 = 开局站位。分配层与叙事感知层都已落地（他人称谓「唐三（户部主事）」+ 本人 self_identities 进引擎上下文），但按 §0.1 平权红线「永不进数值层」：不改判定、不改发奖、不开权限、不调难度、不改准入。",
            "warning": "本页只呈现分配结果，不构成效果验证。「身份池调整 → 戏份分布变化」的读数在 GET /admin/metrics/overview 的 narrativeSlo.calibration.dimensions.identityShareBalance：它按身份 id 分组，给出每个身份的「相对均分倍率」均值与零分观察数（SLO 的叙事注意力基尼按 character_id 聚合，答不了这个问题）。🔴 那份读数是只读观测，不回灌引擎，也不给「配得对不对」的判语——调整身份池后仍须以仿真试跑 + 世界质量回归判断影响，不要拿本页的分布图当「调好了」的证据。",
        },
        "editable": false,
        "editPath": "身份池的唯一写入路径是模板骨架：POST /admin/world-templates 的 skeletonJson.identityPool（新建模板）。本端点只读，不提供在线改配额 / 改主题词。已开出的世界其分配在装配时即钉死在 assembled_json，改模板不会回溯改写既有实例。",
        "notes": [
            "fillRatio 分母 = quota × worldsWithAssignments（只算真的参与过分配的世界；未装配世界不进分母）。",
            "quota 是上限不是保底：人少于 Σquota 时槽位空置，人多于 Σquota 时余下角色不分配身份，两者都不是错误。",
            "activeMembersWithoutIdentity 含「装配之后才入场」的成员——他们本就不在那次分配的名单里。",
            "gini 对声明池内各身份的「原始分配人次」求集中度（0=均分，越大越集中），未按 quota 归一化；配额不等的池请配合 fillRatio 一起读。",
            "unknownIdentityIds 非空 = 老实例钉着模板已删除的身份 id，叙事层对这些角色退化为只显示名字。",
        ],
    })))
}

// ============================================================================
// 维度三：境界档（skeleton.realmTier → assembled_json./assembly/realmTier）
// 总规格 §6【拍板 3】「戏服原则——境界即布景」
// ============================================================================
//
// 🔴 **它与身份池是两种东西，界面上不许混着读**：
//   - 身份池（§5）：**各不相同**的开局站位，有池、有配额、有种子分配 → 该看「分布 / 是否失衡」；
//   - 境界档（§6）：**全员统一**的一件戏服，无池、无配额、**零抽样** → 该看
//     「这一阶有没有戏服、同系列各阶是不是在换戏服、已开出的实例钉住了没有」。
//
// 🔴 **本维度的效力必须说清楚，不许只画一张「已配置」的绿标**：声明层、钉住层、叙事感知层
// 都已落地（`runtime::parse_realm_costume` → 入场导演 prompt），但**只有 `briefing` 与
// `flavorNotes` 进模型上下文**，且它只改描写、不改判定；校准闭环仍然是空的。
// 详见 `realm_tier_effect()` 下发的五层自述。

/// 境界档的**只读投影**（同 `IdentityEntry` 的哲学：对脏数据宽容，照样报出来供运营发现）。
struct RealmEntry {
    id: String,
    label: String,
    cosmology: String,
    genre: String,
    conflict_intensity: String,
    briefing: String,
    flavor_notes: Vec<String>,
}

impl RealmEntry {
    /// 展示名：label 优先，空则回落 id（与 `assembly::identity_display` 同款口径）。
    fn display(&self) -> String {
        if self.label.is_empty() {
            self.id.clone()
        } else {
            self.label.clone()
        }
    }

    /// 取值域自检：三项枚举里有几项是「填了但不在官方枚举内」。建模板端点拦得住，
    /// 历史数据与直写库拦不住 —— 校准面的价值之一就是把它们摆到运营面前。
    /// **留空不算问题**（§6：空体系 = 无战力体系题材，境界泛化为处境）。
    fn invalid_enums(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.cosmology.is_empty()
            && !crate::admission::KNOWN_COSMOLOGIES.contains(&self.cosmology.as_str())
        {
            out.push("cosmology");
        }
        if !self.genre.is_empty() && !crate::assembly::KNOWN_GENRES.contains(&self.genre.as_str()) {
            out.push("genre");
        }
        if !self.conflict_intensity.is_empty()
            && !crate::assembly::KNOWN_CONFLICT_INTENSITIES.contains(&self.conflict_intensity.as_str())
        {
            out.push("conflictIntensity");
        }
        out
    }
}

/// 解析 `skeleton_json.realmTier`。缺省 / 结构不符 / 坏 JSON → `None`（老模板零影响）。
/// **不做取值域过滤**：非法枚举照样解出来，交给 `invalid_enums()` 报成问题项。
fn parse_realm_tier(skeleton_raw: &str) -> Option<RealmEntry> {
    let v: Value = serde_json::from_str(skeleton_raw).ok()?;
    let rt = v.get("realmTier")?;
    if !rt.is_object() {
        return None;
    }
    let s = |k: &str| rt.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    Some(RealmEntry {
        id: s("id"),
        label: s("label"),
        cosmology: s("cosmology"),
        genre: s("genre"),
        conflict_intensity: s("conflictIntensity"),
        briefing: s("briefing"),
        flavor_notes: rt
            .get("flavorNotes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 从实例 `assembled_json` 读回钉住的境界档 `(id, label)`。
///
/// 结构口径与 `assembly::AssembledInstance.realm_tier` 一致，但同 `parse_assignments` 一样
/// **刻意不跨模块复用类型**：观测面挂了不该有能力影响装配 / tick，两边各自防御式解析正是要的隔离。
/// 未装配 / 坏 JSON / 无该键 → `None`（后者是绝大多数存量实例的真实状态，不是错误）。
fn parse_pinned_realm(assembled_json: Option<&str>) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(assembled_json?).ok()?;
    let rt = v.pointer("/assembly/realmTier")?;
    let id = rt.get("id").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if id.is_empty() {
        return None;
    }
    let label = rt.get("label").and_then(Value::as_str).unwrap_or("").trim().to_string();
    Some((id, label))
}

/// 🔴 境界档的效力自述（五层）。前端必须显式渲染，不得只画一张"已配置"的绿标。
///
/// 与身份池的四层相比多一层（「钉住层」：戏服随实例钉死在 `assembled_json`）。
/// 叙事感知层已由 `runtime::parse_realm_costume` 接通到入场导演 prompt——但**只接了描写这一头**，
/// 而且**校准闭环仍然是空的**：没有任何指标能回答「换一件戏服，叙事真的变了吗」。
fn realm_tier_effect() -> Value {
    json!({
        "declarationLayer": "Implemented",
        "pinningLayer": "Implemented",
        "narrativeLayer": "Integrated",
        "numericLayer": "NeverByDesign",
        // 2026-07-27：`slo::calibration` 的戏服维读数上线后，本层从 Missing 转为 Implemented。
        // 🔴 它的形状与身份维**刻意不同**：境界档全员统一（§6），没有组内分布，只能跨世界对比。
        // 同样只到 Implemented：能测了 ≠ 已验证配得对。
        "calibrationLoop": "Implemented",
        // 🔴 下发文案一律**纯文本**：前端把它当普通字符串渲染，写 Markdown 星号只会在界面上
        // 露出两个字面的 `**`。强调一律用中文引号「」。
        "summary": "境界档 = 世界发给全员的同一件戏服（总规格 §6「境界跟着副本走，不跟着角色走」）。它全员统一、无配额、装配层零抽样，只是把模板声明原样钉进实例 assembled_json，再由 runtime 把其中的 briefing 与 flavorNotes 喂给每拍的入场导演。与身份池正相反：身份各不相同，境界人人一样。",
        "warning": "叙事感知层已接通，但只接通了「描写」这一头：七个字段里只有 briefing 与 flavorNotes 进入引擎的入场导演 prompt，改变的是这一篇被怎么写（大家什么水位、招式译成什么风味）；id 与 label 只用于本页展示与审计，cosmology 与 genre 只是取值域标注，conflictIntensity 刻意不进模型上下文——世界是否致命由建房参数 lethality 独立决定，与它无关。境界档一个数字都没有，也永远不改判定、发奖、权限、难度与准入。「换一件戏服，那批世界演得怎么样」的读数在 GET /admin/metrics/overview 的 narrativeSlo.calibration.dimensions.realmTierWorldQuality：按钉住的戏服分桶，各桶各自报完读率 / 阻断率 / 结局分布，另留没钉戏服的世界作对照桶。🔴 那是跨世界对比不是组内分布（境界档全员统一，组内分布恒为退化），且只给分维度的事实、不给综合评分，所以本页仍然不回答「这件戏服配得对不对」。",
    })
}

/// GET /admin/realm-tiers：**声明了境界档的模板目录**。
///
/// 与身份池目录的一处刻意差别：本端点**同时报未声明的模板数**。
/// 身份池未声明 = 这一维零影响，属正常状态；而 §6 说「阶段天然携带境界档——你选阶段，
/// 就是在选境界」，所以一个**归属于某个 Saga 的阶段模板没有境界档，本身就是校准缺口**。
/// 两者分开计数（`undeclaredInSagaCount` / `undeclaredStandaloneCount`），不混为一谈。
pub(super) async fn list_realm_tiers(
    State(state): State<AppState>,
    admin: AdminUser,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;

    let mut cursor = String::new();
    let mut scanned = 0usize;
    let mut items: Vec<Value> = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    let mut undeclared_in_saga = 0i64;
    let mut undeclared_standalone = 0i64;
    let mut truncated = false;
    loop {
        let page = sqlx::query(
            "SELECT id, title, room_type, moderation, saga_id, stage_no, skeleton_json \
             FROM world_templates WHERE id > $1 ORDER BY id ASC LIMIT $2",
        )
        .bind(&cursor)
        .bind(SCAN_PAGE)
        .fetch_all(&state.db)
        .await?;
        if page.is_empty() {
            break;
        }
        for r in &page {
            let id: String = r.try_get("id")?;
            cursor = id.clone();
            scanned += 1;
            let raw: String = r.try_get("skeleton_json")?;
            let saga_id: String = r.try_get("saga_id")?;
            let Some(rt) = parse_realm_tier(&raw) else {
                if saga_id.is_empty() {
                    undeclared_standalone += 1;
                } else {
                    undeclared_in_saga += 1;
                }
                continue;
            };
            ids.push(id.clone());
            let invalid = rt.invalid_enums();
            items.push(json!({
                "templateId": id,
                "title": r.try_get::<String, _>("title")?,
                "roomType": r.try_get::<String, _>("room_type")?,
                "moderation": r.try_get::<String, _>("moderation")?,
                "sagaId": saga_id,
                "stageNo": r.try_get::<i64, _>("stage_no")?,
                "tierId": rt.id,
                "label": rt.display(),
                "cosmology": rt.cosmology,
                "genre": rt.genre,
                "conflictIntensity": rt.conflict_intensity,
                "flavorNoteCount": rt.flavor_notes.len() as i64,
                "invalidEnumFields": invalid,
            }));
        }
        if scanned >= TEMPLATE_DIRECTORY_SCAN_MAX {
            truncated = page.len() as i64 == SCAN_PAGE;
            break;
        }
    }

    let counts = world_counts_by_template(&state.db, &ids).await?;
    for item in &mut items {
        let id = item["templateId"].as_str().unwrap_or_default().to_string();
        let (n, live) = counts.get(&id).copied().unwrap_or((0, 0));
        item["worldCount"] = json!(n);
        item["liveWorldCount"] = json!(live);
    }

    Ok(Json(json!({
        "templates": items,
        "scannedTemplates": scanned as i64,
        "truncated": truncated,
        // 校准缺口的分子分母：归属系列却没戏服的阶段，是真正要补的那批。
        "undeclaredInSagaCount": undeclared_in_saga,
        "undeclaredStandaloneCount": undeclared_standalone,
        "effect": realm_tier_effect(),
        "editable": false,
        "editPath": "境界档只在建模板时录入：POST /admin/world-templates 的 skeletonJson.realmTier。本端点只读，不提供在线改档 / 换戏服。",
        "notes": [
            "只列出声明了 realmTier 对象的模板；其余模板在境界这一维上零影响（装配产物逐字节不变）。",
            "undeclaredInSagaCount 是校准缺口：总规格 §6「阶段天然携带境界档」，归属某个 Saga 的阶段本应各有一件戏服。",
            "undeclaredStandaloneCount 只作对照，不是缺口：独立模板（非 Saga 阶段）没有戏服是正常的。",
            "invalidEnumFields 非空 = 该字段填了官方枚举外的自由文本（建模板端点会拦，出现在此说明是历史数据或直写库）。",
            "cosmology 留空不是缺数据：§6「无战力体系题材（都市/言情/历史），境界泛化为处境」，留空即此意。",
        ],
    })))
}

/// GET /admin/world-templates/{id}/realm-tier?limit=：**境界档声明 + 同系列各阶对照 + 实例钉住情况**。
///
/// 三段回答三个问题：
/// 1. `declaration`：这一阶发的是什么戏服（含取值域自检）；
/// 2. `sagaStages`：**同一系列各阶段是不是真的在换戏服**——§6「你选阶段，就是在选境界」，
///    若各阶同档或多阶缺档，这句话就不成立，是本维度最该被发现的校准问题；
/// 3. `pinning`：这个模板已开出的实例里，有几个真的钉住了境界档、有几个钉的是**旧档**
///    （模板改版后老实例不回溯，同 `unknownIdentityIds` 的处境）。
pub(super) async fn template_realm_tier(
    State(state): State<AppState>,
    admin: AdminUser,
    Path(template_id): Path<String>,
    Query(q): Query<WorldScanQuery>,
) -> Result<Json<Value>, ApiError> {
    require_role(&admin, &["operator"])?;
    let scan = q.limit.unwrap_or(WORLD_SCAN_DEFAULT).clamp(1, WORLD_SCAN_MAX);

    let tpl = sqlx::query(
        "SELECT id, title, room_type, version, moderation, saga_id, stage_no, skeleton_json \
         FROM world_templates WHERE id = $1",
    )
    .bind(&template_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(ApiError::NotFound)?;

    let skeleton_raw: String = tpl.try_get("skeleton_json")?;
    let saga_id: String = tpl.try_get("saga_id")?;
    let realm = parse_realm_tier(&skeleton_raw);

    let declaration = match &realm {
        None => Value::Null,
        Some(rt) => json!({
            "tierId": rt.id,
            "label": rt.display(),
            "cosmology": rt.cosmology,
            "genre": rt.genre,
            "conflictIntensity": rt.conflict_intensity,
            "briefing": rt.briefing,
            "flavorNotes": rt.flavor_notes,
            "invalidEnumFields": rt.invalid_enums(),
            "blankTierId": rt.id.is_empty(),
            // §6「历史题材涉真实人物走更严审核档（合规）」。
            // 🔴 状态 = Concept：**没有接进任何审核链路**，纯提示。
            "stricterModerationHint": crate::assembly::STRICTER_MODERATION_GENRES.contains(&rt.genre.as_str()),
        }),
    };

    // ---- 同系列各阶对照（独立模板 → 空段，不是错误）----
    let mut stages: Vec<Value> = Vec::new();
    let mut stages_without: Vec<i64> = Vec::new();
    let mut tier_stage_map: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut cosmologies: BTreeSet<String> = BTreeSet::new();
    let mut genres: BTreeSet<String> = BTreeSet::new();
    if !saga_id.is_empty() {
        let rows = sqlx::query(
            "SELECT id, title, stage_no, skeleton_json FROM world_templates \
             WHERE saga_id = $1 ORDER BY stage_no ASC, id ASC LIMIT $2",
        )
        .bind(&saga_id)
        .bind(SAGA_DETAIL_TEMPLATE_MAX)
        .fetch_all(&state.db)
        .await?;
        for r in &rows {
            let sid: String = r.try_get("id")?;
            let stage_no: i64 = r.try_get("stage_no")?;
            let raw: String = r.try_get("skeleton_json")?;
            let rt = parse_realm_tier(&raw);
            match &rt {
                Some(rt) if !rt.id.is_empty() => {
                    tier_stage_map.entry(rt.id.clone()).or_default().push(stage_no);
                    if !rt.cosmology.is_empty() {
                        cosmologies.insert(rt.cosmology.clone());
                    }
                    if !rt.genre.is_empty() {
                        genres.insert(rt.genre.clone());
                    }
                }
                _ => stages_without.push(stage_no),
            }
            stages.push(json!({
                "templateId": sid,
                "title": r.try_get::<String, _>("title")?,
                "stageNo": stage_no,
                "declared": rt.is_some(),
                "tierId": rt.as_ref().map(|x| x.id.clone()),
                "label": rt.as_ref().map(RealmEntry::display),
                "cosmology": rt.as_ref().map(|x| x.cosmology.clone()),
                "isSelf": sid == template_id,
            }));
        }
    }
    // 多阶共用同一个档 id：「你选阶段就是在选境界」在这几阶之间不成立。
    let reused: Vec<Value> = tier_stage_map
        .iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(id, v)| json!({ "tierId": id, "stageNos": v }))
        .collect();

    // ---- 实例钉住情况 ----
    let world_rows = sqlx::query(
        "SELECT id, title, status, assembled_json FROM worlds WHERE template_id = $1 \
         ORDER BY created_at DESC, id DESC LIMIT $2",
    )
    .bind(&template_id)
    .bind(scan + 1)
    .fetch_all(&state.db)
    .await?;
    let worlds_truncated = world_rows.len() as i64 > scan;
    let world_rows: Vec<_> = world_rows.into_iter().take(scan as usize).collect();

    let declared_id = realm.as_ref().map(|r| r.id.clone()).unwrap_or_default();
    let mut worlds_assembled = 0i64;
    let mut worlds_with_realm = 0i64;
    let mut stale: BTreeMap<String, i64> = BTreeMap::new();
    let mut world_items: Vec<Value> = Vec::with_capacity(world_rows.len());
    for r in &world_rows {
        let assembled: Option<String> = r.try_get("assembled_json")?;
        let has_assembled = assembled.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
        if has_assembled {
            worlds_assembled += 1;
        }
        let pinned = parse_pinned_realm(assembled.as_deref());
        if pinned.is_some() {
            worlds_with_realm += 1;
        }
        // 「钉的是旧档」只在模板当前**确有声明**时才谈得上：模板本就没声明的话，
        // 实例钉着什么都只是历史，报成 stale 会制造假问题。
        let matches = match (&pinned, declared_id.is_empty()) {
            (Some((pid, _)), false) => Some(pid == &declared_id),
            _ => None,
        };
        if matches == Some(false) {
            if let Some((pid, _)) = &pinned {
                *stale.entry(pid.clone()).or_insert(0) += 1;
            }
        }
        world_items.push(json!({
            "id": r.try_get::<String, _>("id")?,
            "title": r.try_get::<String, _>("title")?,
            "status": r.try_get::<String, _>("status")?,
            "assembled": has_assembled,
            "pinnedTierId": pinned.as_ref().map(|(id, _)| id.clone()),
            "pinnedLabel": pinned.as_ref().map(|(id, label)| {
                if label.is_empty() { id.clone() } else { label.clone() }
            }),
            // null = 模板未声明境界档，"是否与模板一致"这个问题不成立（不得当 false 读）。
            "matchesTemplate": matches,
        }));
    }
    let stale_list: Vec<Value> = stale
        .iter()
        .map(|(id, n)| json!({ "tierId": id, "worldCount": n }))
        .collect();

    Ok(Json(json!({
        "templateId": tpl.try_get::<String, _>("id")?,
        "title": tpl.try_get::<String, _>("title")?,
        "roomType": tpl.try_get::<String, _>("room_type")?,
        "version": tpl.try_get::<i64, _>("version")?,
        "moderation": tpl.try_get::<String, _>("moderation")?,
        "sagaId": saga_id,
        "stageNo": tpl.try_get::<i64, _>("stage_no")?,
        "declared": realm.is_some(),
        "declaration": declaration,
        "sagaStages": {
            "stages": stages,
            "stagesWithoutRealmTier": stages_without,
            "reusedTierIds": reused,
            "distinctCosmologies": cosmologies,
            "distinctGenres": genres,
        },
        "pinning": {
            "worldsScanned": world_rows.len() as i64,
            "worldsTruncated": worlds_truncated,
            "worldsAssembled": worlds_assembled,
            "worldsWithRealmTier": worlds_with_realm,
            "staleTierIds": stale_list,
            "worlds": world_items,
        },
        "effect": realm_tier_effect(),
        "editable": false,
        "editPath": "境界档的唯一写入路径是模板骨架：POST /admin/world-templates 的 skeletonJson.realmTier（新建模板）。本端点只读，不提供在线改档 / 换戏服 / 补写老实例。已开出的世界其境界档在装配时即钉死在 assembled_json，改模板不会回溯改写既有实例。",
        "notes": [
            "境界档全员统一（§6）：它没有配额、没有分配、装配层对它零抽样，所以本页没有「分布」可看——那是身份池（§5）的问题，不是境界的。",
            "sagaStages 只在本模板归属某个 Saga 时非空；独立模板的这一段为空，不是缺数据。",
            "reusedTierIds 非空 = 同一系列多个阶段发同一件戏服，「你选阶段就是在选境界」在这几阶之间不成立，值得复核是不是漏改。",
            "distinctCosmologies 出现多于一个值 = 同一系列跨了体系，按 §6 跨体系应走风味翻译而不是换档，值得复核。",
            "worldsWithRealmTier 少于 worldsAssembled 属正常：在模板声明境界档之前装配的实例不会回溯补写。",
            "staleTierIds = 实例钉着的档 id 与模板当前声明不一致（模板改版后老实例保持原样）。模板未声明时该项恒为空，matchesTemplate 为 null。",
        ],
    })))
}

// ============================================================================
// 纯函数单测（端点级集成测试在 `admin_api/tests.rs`，那里有 build_router / token 等设施）
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺号必须从 1 起算：3-4-5 的系列缺的正是开篇 1、2，从 min 起算会把最该被发现的问题洗掉。
    #[test]
    fn continuity_counts_missing_from_stage_one() {
        let (missing, dup, unnumbered) = stage_continuity(&[3, 4, 5]);
        assert_eq!(missing, vec![1, 2], "缺开篇必须报出来");
        assert!(dup.is_empty());
        assert_eq!(unnumbered, 0);

        let (missing, dup, unnumbered) = stage_continuity(&[1, 2, 3]);
        assert!(missing.is_empty() && dup.is_empty() && unnumbered == 0, "1-2-3 是齐整的");
    }

    /// 重号与未编号各自成诊断：saga_id 非空却 stage_no<=0 是脏数据（建模板端点拦得住，直写库拦不住）。
    #[test]
    fn continuity_reports_duplicates_and_unnumbered() {
        let (missing, dup, unnumbered) = stage_continuity(&[1, 2, 2, 0, -1]);
        assert!(missing.is_empty(), "1-2 连续无缺号");
        assert_eq!(dup, vec![2], "阶段 2 有两个模板");
        assert_eq!(unnumbered, 2, "0 与 -1 都算未编号");
    }

    /// 缺号清单封顶，且封顶时不得再宣称 contiguous 的前置项「missing 为空」。
    #[test]
    fn continuity_truncates_absurd_missing_list() {
        let (missing, _, _) = stage_continuity(&[999]);
        assert_eq!(missing.len(), MISSING_STAGES_MAX, "998 个缺号必须截断成噪声上限");
    }

    /// 老模板（无 identityPool）→ 空池，零影响；`quota` 缺省 1，与装配层默认一致。
    #[test]
    fn identity_pool_parse_defaults_and_absence() {
        assert!(parse_identity_pool(r#"{"mainlineNodes":[]}"#).is_empty(), "无 identityPool → 空池");
        assert!(parse_identity_pool("不是 JSON").is_empty(), "坏 JSON → 空池，不 panic");

        let pool = parse_identity_pool(
            r#"{"identityPool":[
                 {"id":"official","label":"户部主事","quota":3,"themes":["朝堂"],"hookAffinity":["arc-court"]},
                 {"id":"jilted","isLead":true}
               ]}"#,
        );
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0].quota, 3);
        assert_eq!(pool[0].themes, vec!["朝堂".to_string()]);
        assert_eq!(pool[0].hook_affinity, vec!["arc-court".to_string()]);
        assert!(!pool[0].is_lead);
        assert_eq!(pool[1].quota, 1, "quota 缺省必须是 1（同 assembly::IdentitySpec）");
        assert!(pool[1].is_lead);
        assert_eq!(pool[1].label, "", "label 缺省为空，回落由展示层做");
    }

    /// 脏数据要被**保留并报出来**（校准面的价值），不能像装配层那样直接丢弃。
    #[test]
    fn identity_pool_parse_keeps_dirty_entries_for_reporting() {
        let pool = parse_identity_pool(r#"{"identityPool":[{"label":"没有 id"},{"id":"x","quota":0}]}"#);
        assert_eq!(pool.len(), 2, "空 id 与 0 配额都必须留在结果里供运营发现");
        assert_eq!(pool[0].id, "");
        assert_eq!(pool[1].quota, 0);
    }

    /// 分配读回的退化契约：与 `runtime::parse_identity_assignments` 逐条对齐。
    #[test]
    fn assignment_parse_degrades_defensively() {
        assert!(parse_assignments(None).is_empty(), "未装配 → 空");
        assert!(parse_assignments(Some("{")).is_empty(), "坏 JSON → 空");
        assert!(parse_assignments(Some(r#"{"assembly":{}}"#)).is_empty(), "无该字段 → 空");
        assert!(
            parse_assignments(Some(r#"{"assembly":{"identityAssignments":"x"}}"#)).is_empty(),
            "字段类型不符 → 空"
        );
        // 单条损坏只跳过该条，不牵连其余。
        let got = parse_assignments(Some(
            r#"{"assembly":{"identityAssignments":[["chA","official"],["chB"],"x",["","y"],["chC","merchant"]]}}"#,
        ));
        assert_eq!(
            got,
            vec![
                ("chA".to_string(), "official".to_string()),
                ("chC".to_string(), "merchant".to_string())
            ],
            "坏条目逐条跳过，好条目一条不少"
        );
    }

    /// 骨架坏了 → `parsed:false` 且各形状指标**字段缺席**，绝不编 0。
    #[test]
    fn skeleton_shape_distinguishes_broken_from_empty() {
        let broken = skeleton_shape("{{ 不是 JSON");
        assert_eq!(broken["parsed"], false);
        assert!(broken["mainlineNodes"].is_null(), "坏骨架的主线数必须缺席，不得是 0");

        let empty = skeleton_shape("{}");
        assert_eq!(empty["parsed"], true);
        assert_eq!(empty["mainlineNodes"], 0, "合法但空的骨架，0 才是真实答案");
        assert_eq!(empty["hasPayoutTable"], false);

        let full = skeleton_shape(
            r#"{"mainlineNodes":[1,2,3],"endingPool":[1],"payoutTable":{"tiers":[]},
                "identityPool":[{"id":"a","quota":2},{"id":"b","quota":3,"isLead":true}]}"#,
        );
        assert_eq!(full["mainlineNodes"], 3);
        assert_eq!(full["endingPool"], 1);
        assert_eq!(full["hasPayoutTable"], true);
        assert_eq!(full["identityPoolSize"], 2);
        assert_eq!(full["identityQuotaTotal"], 5);
        assert_eq!(full["identityLeadCount"], 1);
    }

    // ---------------- 维度三：境界档 ----------------

    /// 老模板（无 realmTier）→ None，零影响；坏 JSON / 类型不符同样退化为 None，不 panic。
    #[test]
    fn realm_tier_parse_absence_and_garbage() {
        assert!(parse_realm_tier(r#"{"mainlineNodes":[]}"#).is_none(), "无 realmTier → None");
        assert!(parse_realm_tier("不是 JSON").is_none(), "坏 JSON → None，不 panic");
        assert!(parse_realm_tier(r#"{"realmTier":"斗王档"}"#).is_none(), "不是对象 → None");
        assert!(parse_realm_tier(r#"{"realmTier":[]}"#).is_none(), "数组 → None（境界不是池）");
    }

    /// 字段解析 + label 回落 id + 空 flavorNotes 条目被丢掉。
    #[test]
    fn realm_tier_parse_fields_and_display_fallback() {
        let rt = parse_realm_tier(
            r#"{"realmTier":{"id":"tier-douwang","label":" 斗王档 ","cosmology":"cultivation",
                 "genre":"xuanhuan","conflictIntensity":"martial","briefing":"全员斗王水位",
                 "flavorNotes":["魂技译为斗气招式", "  ", ""]}}"#,
        )
        .unwrap();
        assert_eq!(rt.id, "tier-douwang");
        assert_eq!(rt.display(), "斗王档", "两侧空白必须去掉");
        assert_eq!(rt.cosmology, "cultivation");
        assert_eq!(rt.flavor_notes, vec!["魂技译为斗气招式".to_string()], "空条目不进列表");
        assert!(rt.invalid_enums().is_empty(), "全合法枚举不得报问题");

        let bare = parse_realm_tier(r#"{"realmTier":{"id":"tier-x"}}"#).unwrap();
        assert_eq!(bare.display(), "tier-x", "label 空 → 回落 id");
        assert!(bare.invalid_enums().is_empty(), "三项留空是合法的（§6 无战力体系题材）");
    }

    /// 脏数据必须**留在结果里并被报出来**（校准面的价值），不能像装配层那样直接拒绝。
    #[test]
    fn realm_tier_parse_reports_invalid_enums() {
        let rt = parse_realm_tier(
            r#"{"realmTier":{"id":"t","cosmology":"斗气","genre":"宫斗","conflictIntensity":"很凶"}}"#,
        )
        .unwrap();
        assert_eq!(rt.invalid_enums(), vec!["cosmology", "genre", "conflictIntensity"]);

        let blank = parse_realm_tier(r#"{"realmTier":{"label":"没有 id"}}"#).unwrap();
        assert_eq!(blank.id, "", "空 id 条目必须留下供运营发现（装配层是拒绝，校准面是报出来）");
        assert_eq!(blank.display(), "没有 id", "有 label 就用 label，缺 id 另由 blankTierId 报");

        let nothing = parse_realm_tier(r#"{"realmTier":{}}"#).unwrap();
        assert_eq!(nothing.display(), "", "id 与 label 都空 → 展示名为空，不编内容");
    }

    /// 实例侧读回的退化契约：未装配 / 坏 JSON / 无键 / 空 id 一律 None。
    #[test]
    fn pinned_realm_parse_degrades_defensively() {
        assert!(parse_pinned_realm(None).is_none(), "未装配 → None");
        assert!(parse_pinned_realm(Some("{")).is_none(), "坏 JSON → None");
        assert!(parse_pinned_realm(Some(r#"{"assembly":{}}"#)).is_none(), "无该键 → None（存量实例的常态）");
        assert!(
            parse_pinned_realm(Some(r#"{"assembly":{"realmTier":{"id":"  ","label":"无 id"}}}"#)).is_none(),
            "空 id 钉不住任何东西 → None"
        );
        assert_eq!(
            parse_pinned_realm(Some(r#"{"assembly":{"realmTier":{"id":"t1","label":"斗王档"}}}"#)),
            Some(("t1".to_string(), "斗王档".to_string()))
        );
    }

    /// 🔴 效力自述：叙事感知层已接通（`Integrated`），数值层必须**永远**是 `NeverByDesign`，
    /// 校准闭环 = `Implemented`（**读数建成，闭环未验证**）。
    ///
    /// 七档语言（VALIDATION §0.3）在此的边界：接通 = `Integrated`，读数建成 = `Implemented`，
    /// **都不是** `Validated` 更不是 `Enabled` —— 没有任何真实用户数据证明「这样穿戏服更好」。
    /// 谁想把 `calibrationLoop` 往上抬，先拿出「运营据此调过档 + 下一批世界的因果证据」。
    #[test]
    fn realm_tier_effect_states_narrative_layer_is_integrated() {
        let e = realm_tier_effect();
        assert_eq!(e["declarationLayer"], "Implemented");
        assert_eq!(e["pinningLayer"], "Implemented");
        assert_eq!(
            e["narrativeLayer"], "Integrated",
            "runtime::parse_realm_costume → RoundInput.realm_costume → 入场导演 prompt"
        );
        assert_eq!(e["numericLayer"], "NeverByDesign", "§6 跨体系靠风味翻译，不靠数值换算");
        assert_eq!(
            e["calibrationLoop"], "Implemented",
            "slo::calibration 的戏服维读数已建成；但它只是「能测了」，不许标到 Validated"
        );
        // 🔴 七档天花板：接通之后最多到 Integrated，任何一层都不许出现「已验证 / 可上线」口径。
        for k in ["declarationLayer", "pinningLayer", "narrativeLayer", "numericLayer", "calibrationLoop"] {
            let v = e[k].as_str().unwrap();
            assert!(
                !matches!(v, "Production-ready" | "Validated" | "Enabled"),
                "{k} = {v}：境界档没有任何真实用户数据，不得标到 Integrated 之上"
            );
        }
        // 下发文案必须是纯文本（前端按普通字符串渲染，Markdown 星号会在界面上露出字面量）。
        for k in ["summary", "warning"] {
            assert!(!e[k].as_str().unwrap().contains("**"), "{k} 混入了 Markdown 强调");
        }
    }

    /// 骨架形状里境界档是**布尔 + 档名**，不是规模数字（§6 全员统一，没有"几个境界"这回事）。
    #[test]
    fn skeleton_shape_reports_realm_tier_as_presence_not_size() {
        let none = skeleton_shape(r#"{"mainlineNodes":[]}"#);
        assert_eq!(none["hasRealmTier"], false);
        assert!(none["realmTierLabel"].is_null(), "未声明 → null，不是空串");

        let some = skeleton_shape(r#"{"realmTier":{"id":"t1","label":"斗王档"}}"#);
        assert_eq!(some["hasRealmTier"], true);
        assert_eq!(some["realmTierLabel"], "斗王档");
    }
}
