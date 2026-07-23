//! 物品体系标签与世界准入（S4）：平台规格 §9.5.B。
//! 准入执行是服务端双重校验：入场时过滤携带清单（backpack::carry）+ 运行时仲裁拒绝未准入物品使用。
//! 本模块只提供纯函数判定与结构化转译，不触库、不触网——判定可单测覆盖全分支。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemOrigin {
    pub world_template_id: String,
    /// 官方维护的有限体系枚举：magic / tech / cultivation / mundane / ...（自由文本拒绝）
    pub cosmology: Vec<String>,
    pub power_tier: u8, // 1–5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDefinition {
    pub id: String,
    pub narrative: String,
    pub effect_tags: Vec<String>,
    pub origin: ItemOrigin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionMode {
    #[default]
    Open,
    Denylist,
    Allowlist,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RejectedHandling {
    #[default]
    StayInBackpack,
    SealedInside,
    Translate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldAdmissionPolicy {
    #[serde(default)]
    pub mode: AdmissionMode,
    #[serde(default)]
    pub cosmologies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_power_tier: Option<u8>,
    #[serde(default)]
    pub rejected_handling: RejectedHandling,
}

impl Default for WorldAdmissionPolicy {
    fn default() -> Self {
        Self {
            mode: AdmissionMode::Open,
            cosmologies: Vec::new(),
            max_power_tier: None,
            rejected_handling: RejectedHandling::StayInBackpack,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AdmissionDecision {
    Admitted,
    /// 拒收：留在账号背包
    Rejected,
    /// 入场但封存
    Sealed,
    /// 规则转译：叙事外皮重写，effectTags/powerTier 不变或降档（转译文案由装配器生成）
    Translated,
}

/// 官方体系枚举白名单（后台可配的最小集，非法标签视为校验错误）。
pub const KNOWN_COSMOLOGIES: &[&str] = &["magic", "tech", "cultivation", "mundane", "psychic", "myth"];

fn is_known(tag: &str) -> bool {
    KNOWN_COSMOLOGIES.contains(&tag)
}

/// 校验一组体系标签均在官方枚举内（自由文本 → Validation）。
fn validate_cosmologies(tags: &[String], ctx: &str) -> Result<(), muse_engine::EngineError> {
    for t in tags {
        if !is_known(t) {
            return Err(muse_engine::EngineError::Validation(format!(
                "未知体系标签 `{t}`（{ctx}）；仅允许 {KNOWN_COSMOLOGIES:?}"
            )));
        }
    }
    Ok(())
}

/// 纯函数判定（§9.5.B）：
/// - open 全收（体系不设限）；denylist 命中体系拒；allowlist 未全列拒；
/// - powerTier 超 maxPowerTier 拒（与 mode 正交，任何模式都执行强度上限）；
/// - 被拒结果按 rejectedHandling 落地：stay_in_backpack→Rejected / sealed_inside→Sealed / translate→Translated；
/// - 物品或策略标签不在 KNOWN_COSMOLOGIES → Validation 错误。
pub fn check_admission(
    policy: &WorldAdmissionPolicy,
    item: &ItemDefinition,
) -> Result<AdmissionDecision, muse_engine::EngineError> {
    // 1) 标签白名单校验（物品来源体系 + 策略作用对象都不接受自由文本）。
    validate_cosmologies(&item.origin.cosmology, "item.origin.cosmology")?;
    validate_cosmologies(&policy.cosmologies, "policy.cosmologies")?;

    // 2) 体系闸门。
    let cosmology_ok = match policy.mode {
        AdmissionMode::Open => true,
        // 命中黑名单任一体系即拒。
        AdmissionMode::Denylist => !item
            .origin
            .cosmology
            .iter()
            .any(|c| policy.cosmologies.iter().any(|p| p == c)),
        // 白名单：物品须有体系标签且全部在白名单内；未列（含无标签）即拒。
        AdmissionMode::Allowlist => {
            !item.origin.cosmology.is_empty()
                && item
                    .origin
                    .cosmology
                    .iter()
                    .all(|c| policy.cosmologies.iter().any(|p| p == c))
        }
    };

    // 3) 强度闸门（与 mode 正交）。
    let power_ok = match policy.max_power_tier {
        Some(max) => item.origin.power_tier <= max,
        None => true,
    };

    if cosmology_ok && power_ok {
        return Ok(AdmissionDecision::Admitted);
    }

    // 4) 被拒 → 按世界声明的处理方式落地。
    Ok(match policy.rejected_handling {
        RejectedHandling::StayInBackpack => AdmissionDecision::Rejected,
        RejectedHandling::SealedInside => AdmissionDecision::Sealed,
        RejectedHandling::Translate => AdmissionDecision::Translated,
    })
}

/// 结构化转译（§9.5.B 强度后门防线）：只降档不升档。
/// - `effect_tags` 恒不变（仲裁器只认标签，转译不得改变规则效果）；
/// - `power_tier` 若超过 maxPowerTier 则夹到上限，否则保持原值；
/// - 叙事外皮的实际重写由装配器（数次模型/规则调用）生成，这里只做可验证的数值降档。
pub fn translate_item(policy: &WorldAdmissionPolicy, item: &ItemDefinition) -> ItemDefinition {
    let mut t = item.clone();
    if let Some(max) = policy.max_power_tier {
        if t.origin.power_tier > max {
            t.origin.power_tier = max; // 降档不升
        }
    }
    // effect_tags 原样保留（防止转译成为强度后门）。
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(cosmology: &[&str], tier: u8) -> ItemDefinition {
        ItemDefinition {
            id: "itm".into(),
            narrative: "一把会呼吸的剑".into(),
            effect_tags: vec!["advantage:combat".into(), "reroll:once".into()],
            origin: ItemOrigin {
                world_template_id: "tpl".into(),
                cosmology: cosmology.iter().map(|s| s.to_string()).collect(),
                power_tier: tier,
            },
        }
    }

    fn policy(mode: AdmissionMode, cos: &[&str], max: Option<u8>, h: RejectedHandling) -> WorldAdmissionPolicy {
        WorldAdmissionPolicy {
            mode,
            cosmologies: cos.iter().map(|s| s.to_string()).collect(),
            max_power_tier: max,
            rejected_handling: h,
        }
    }

    #[test]
    fn open_admits_everything_including_high_tier() {
        let p = policy(AdmissionMode::Open, &[], None, RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&["tech"], 5)).unwrap(), AdmissionDecision::Admitted);
    }

    #[test]
    fn denylist_hit_is_rejected() {
        let p = policy(AdmissionMode::Denylist, &["tech"], None, RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&["tech"], 2)).unwrap(), AdmissionDecision::Rejected);
    }

    #[test]
    fn denylist_miss_is_admitted() {
        let p = policy(AdmissionMode::Denylist, &["tech"], None, RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&["magic"], 2)).unwrap(), AdmissionDecision::Admitted);
    }

    #[test]
    fn allowlist_fully_listed_is_admitted() {
        let p = policy(AdmissionMode::Allowlist, &["magic", "myth"], None, RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&["magic"], 2)).unwrap(), AdmissionDecision::Admitted);
    }

    #[test]
    fn allowlist_unlisted_is_rejected() {
        let p = policy(AdmissionMode::Allowlist, &["magic"], None, RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&["tech"], 2)).unwrap(), AdmissionDecision::Rejected);
    }

    #[test]
    fn allowlist_untagged_item_is_rejected() {
        let p = policy(AdmissionMode::Allowlist, &["magic"], None, RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&[], 1)).unwrap(), AdmissionDecision::Rejected);
    }

    #[test]
    fn power_tier_over_limit_is_rejected_in_any_mode() {
        // 体系放行但强度超限 → 拒。
        let p = policy(AdmissionMode::Open, &[], Some(3), RejectedHandling::StayInBackpack);
        assert_eq!(check_admission(&p, &item(&["magic"], 5)).unwrap(), AdmissionDecision::Rejected);
        // 恰好等于上限 → 放行。
        assert_eq!(check_admission(&p, &item(&["magic"], 3)).unwrap(), AdmissionDecision::Admitted);
    }

    #[test]
    fn translate_mode_turns_rejection_into_translated() {
        let p = policy(AdmissionMode::Denylist, &["tech"], None, RejectedHandling::Translate);
        assert_eq!(check_admission(&p, &item(&["tech"], 2)).unwrap(), AdmissionDecision::Translated);
    }

    #[test]
    fn sealed_mode_turns_rejection_into_sealed() {
        let p = policy(AdmissionMode::Denylist, &["tech"], None, RejectedHandling::SealedInside);
        assert_eq!(check_admission(&p, &item(&["tech"], 2)).unwrap(), AdmissionDecision::Sealed);
    }

    #[test]
    fn unknown_cosmology_is_validation_error() {
        let p = policy(AdmissionMode::Open, &[], None, RejectedHandling::StayInBackpack);
        let err = check_admission(&p, &item(&["timelord"], 2)).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn unknown_policy_cosmology_is_validation_error() {
        let p = policy(AdmissionMode::Denylist, &["warp"], None, RejectedHandling::StayInBackpack);
        let err = check_admission(&p, &item(&["magic"], 2)).unwrap_err();
        assert_eq!(err.code(), "validation");
    }

    #[test]
    fn translate_only_downgrades_never_upgrades_and_keeps_effect_tags() {
        let p = policy(AdmissionMode::Denylist, &["tech"], Some(3), RejectedHandling::Translate);
        let original = item(&["tech"], 5);
        let translated = translate_item(&p, &original);
        // effectTags 不变。
        assert_eq!(translated.effect_tags, original.effect_tags);
        // powerTier 只降不升。
        assert!(translated.origin.power_tier <= original.origin.power_tier);
        assert_eq!(translated.origin.power_tier, 3);

        // 上限内的物品：powerTier 保持不变（不改数值）。
        let low = item(&["tech"], 2);
        let low_t = translate_item(&p, &low);
        assert_eq!(low_t.origin.power_tier, 2);
        assert_eq!(low_t.effect_tags, low.effect_tags);
    }

    #[test]
    fn default_policy_parses_from_minimal_json() {
        // 迁移默认值 {"mode":"open"} 缺 rejectedHandling 也能解析（serde default）。
        let p: WorldAdmissionPolicy = serde_json::from_str(r#"{"mode":"open"}"#).unwrap();
        assert_eq!(p.mode, AdmissionMode::Open);
        assert_eq!(p.rejected_handling, RejectedHandling::StayInBackpack);
    }
}
