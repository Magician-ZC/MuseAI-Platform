use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};
use walkdir::WalkDir;

use crate::models::*;
use crate::utils::*;

const BUILTIN_SKILL_NAMES: &[&str] = &[
    "kitt-writer",
    "fanqie-short-zhuiqi-outline",
    "fanqie-short-zhuiqi-writer",
    "fanqie-xuanhuan-outline",
    "fanqie-xuanhuan-writer",
    "fanqie-short-danvzhu-outline",
    "fanqie-short-danvzhu-writer",
    "fanqie-short-guize-outline",
    "fanqie-short-guize-writer",
    "fanqie-short-qianjin-outline",
    "fanqie-short-qianjin-writer",
    "fanqie-short-xitong-outline",
    "fanqie-short-xitong-writer",
    "fanqie-long-xianxia-outline",
    "fanqie-long-xianxia-writer",
    "fanqie-long-xifang-outline",
    "fanqie-long-xifang-writer",
    "fanqie-long-lishi-outline",
    "fanqie-long-lishi-writer",
    "fanqie-long-youxi-outline",
    "fanqie-long-youxi-writer",
];

static BUILTIN_SKILLS_SYNCED: OnceLock<()> = OnceLock::new();

pub fn discover_skills(app: Option<&AppHandle>) -> Vec<SkillDefinition> {
    let mut roots = Vec::new();
    if let Some(app_handle) = app {
        if let Ok(dir) = app_handle.path().app_data_dir() {
            let skills_dir = dir.join("skills");
            ensure_builtin_skills_synced(app_handle, &skills_dir);
            roots.push(skills_dir);
        }
    }

    let mut skills = Vec::new();
    for root in roots {
        if root.join("SKILL.md").is_file() {
            if let Some(skill) = parse_skill_definition(&root) {
                skills.push(skill);
            }
        } else {
            collect_skills_from_root(&root, &mut skills);
        }
    }
    skills
}
fn ensure_builtin_skills_synced(app: &AppHandle, skills_dir: &Path) {
    BUILTIN_SKILLS_SYNCED.get_or_init(|| {
        if let Err(error) = sync_builtin_skills(app, skills_dir) {
            eprintln!("同步内置 Skill 失败: {}", error);
        }
    });
}
fn sync_builtin_skills(app: &AppHandle, skills_dir: &Path) -> Result<(), String> {
    let source_dir = builtin_skills_source_dir(app)?;
    fs::create_dir_all(skills_dir).map_err(|e| e.to_string())?;

    // 清理已被废弃/重命名的旧内置技能
    let obsolete_skills = vec!["fanqie-short-nuexin-outline", "fanqie-short-nuexin-writer"];
    for skill_name in obsolete_skills {
        let obsolete_path = skills_dir.join(skill_name);
        if obsolete_path.exists() {
            if obsolete_path.is_dir() {
                let _ = fs::remove_dir_all(&obsolete_path);
            } else {
                let _ = fs::remove_file(&obsolete_path);
            }
        }
    }

    for skill_name in BUILTIN_SKILL_NAMES {
        let source = source_dir.join(skill_name);
        if !source.join("SKILL.md").is_file() {
            return Err(format!("内置 Skill '{}' 缺少 SKILL.md", skill_name));
        }

        let dest = skills_dir.join(skill_name);
        if dest.exists() {
            if dest.is_dir() {
                fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
            } else {
                fs::remove_file(&dest).map_err(|e| e.to_string())?;
            }
        }
        copy_dir_recursive(&source, &dest).map_err(|e| e.to_string())?;
    }

    Ok(())
}
fn builtin_skills_source_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let bundled = app
        .path()
        .resolve("skills", BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;

    let mut bundled_has_all = true;
    if bundled.is_dir() {
        for skill_name in BUILTIN_SKILL_NAMES {
            if !bundled.join(skill_name).join("SKILL.md").is_file() {
                bundled_has_all = false;
                break;
            }
        }
    } else {
        bundled_has_all = false;
    }

    if bundled_has_all {
        return Ok(bundled);
    }

    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join("skills");
    if dev.is_dir() {
        return Ok(dev);
    }

    Err(format!("未找到内置 Skill 资源目录: {}", bundled.display()))
}
fn collect_skills_from_root(root: &Path, skills: &mut Vec<SkillDefinition>) {
    if !root.is_dir() {
        return;
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.join("SKILL.md").is_file() {
            if let Some(skill) = parse_skill_definition(&path) {
                skills.push(skill);
            }
            continue;
        }
        if path.is_dir() {
            let Ok(nested_entries) = fs::read_dir(path) else {
                continue;
            };
            for nested in nested_entries.flatten() {
                let nested_path = nested.path();
                if nested_path.join("SKILL.md").is_file() {
                    if let Some(skill) = parse_skill_definition(&nested_path) {
                        skills.push(skill);
                    }
                }
            }
        }
    }
}
fn parse_skill_definition(path: &Path) -> Option<SkillDefinition> {
    let body = fs::read_to_string(path.join("SKILL.md")).ok()?;
    let name = extract_frontmatter_value(&body, "name").or_else(|| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(String::from)
    })?;
    let description = extract_frontmatter_value(&body, "description").unwrap_or_default();
    Some(SkillDefinition {
        name,
        description,
        path: path.to_path_buf(),
    })
}
pub fn list_skill_files(root: &Path) -> Vec<String> {
    let mut files = vec![root.join("SKILL.md").display().to_string()];
    for entry in WalkDir::new(root).max_depth(3).into_iter().flatten() {
        let path = entry.path();
        if !path.is_file() || path.file_name().is_some_and(|name| name == "SKILL.md") {
            continue;
        }
        files.push(path.display().to_string());
        if files.len() >= 24 {
            break;
        }
    }
    files
}
/// 技能名会被当成目录名拼进路径（导入时写、删除时 `remove_dir_all`），
/// 而它来自被导入的 `SKILL.md` frontmatter —— 技能包**支持用户导入**，即完全由不受信内容决定。
/// 判据复用全仓唯一那一处（`utils::validated_path_component`），不在这里另写一份。
fn validated_skill_dir_name(name: &str) -> Result<&str, String> {
    crate::utils::validated_path_component(name, "Skill 名称")
}

#[tauri::command]
pub fn import_skill(app: AppHandle, path: String) -> Result<SkillDefinition, String> {
    let source = Path::new(&path);
    if !source.is_dir() {
        return Err("指定的路径不是一个有效的文件夹".to_string());
    }
    if !source.join("SKILL.md").is_file() {
        return Err("文件夹中未找到 SKILL.md 文件".to_string());
    }

    let skill =
        parse_skill_definition(source).ok_or_else(|| "无法解析 SKILL.md 信息".to_string())?;
    // 🔴 先校验再拼路径：`name` 来自被导入内容，未校验就 join 等于把写入位置交给对方决定。
    let safe_name = validated_skill_dir_name(&skill.name)?;

    let skills_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("skills");
    let dest = skills_dir.join(safe_name);

    if dest.exists() {
        return Err(format!(
            "Skill '{}' 已存在，请先删除旧版本或重命名。",
            skill.name
        ));
    }

    copy_dir_recursive(source, &dest).map_err(|e| format!("复制文件夹失败: {}", e))?;

    parse_skill_definition(&dest).ok_or_else(|| "成功复制，但验证解析失败".to_string())
}
#[tauri::command]
pub fn delete_skill(app: AppHandle, name: String) -> Result<(), String> {
    // 🔴 同一道校验（删除比导入更危险：它 `remove_dir_all`，而技能名可能是导入时带进来的）。
    let safe_name = validated_skill_dir_name(&name)?;
    let dest = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("skills")
        .join(safe_name);
    if !dest.exists() {
        return Err(format!("Skill '{}' 不存在", name));
    }
    fs::remove_dir_all(&dest).map_err(|e| e.to_string())
}
#[tauri::command]
pub fn get_skills(app: AppHandle) -> Result<Vec<SkillDefinition>, String> {
    Ok(discover_skills(Some(&app)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 🔴 **技能名是路径分量，穿越写法必须一条不漏地拒掉。**
    ///
    /// `name` 来自被导入的 `SKILL.md` frontmatter（技能包支持用户导入 = 不受信内容），
    /// 此前**未经任何校验**就进了 `skills_dir.join(&skill.name)`：
    /// 导入时写到 skills 目录之外，删除时 `remove_dir_all` 把那个目录树删掉。
    /// ⚠️ 绝对路径尤其致命——Rust 的 `Path::join` 遇到绝对路径会**整个替换基路径**，连 `..` 都不需要。
    #[test]
    fn skill_name_must_be_a_single_plain_directory_component() {
        for bad in [
            "../evil",
            "../../Documents",
            "a/b",
            "a\\b",
            "/tmp/evil",
            "/Users/x/.ssh",
            "..",
            ".",
            ".hidden",
            "",
            "   ",
        ] {
            assert!(
                validated_skill_dir_name(bad).is_err(),
                "🔴 非法技能名被放行（会写/删到 skills 目录之外）：{bad:?}"
            );
        }
    }

    /// 🔴 **反方向同样要钉**：校验不得把合法技能挡在外面。
    /// 技能名可以是中文、可以带空格与连字符——用白名单字符集会误伤，而挡住穿越并不需要那么严。
    #[test]
    fn skill_name_validation_does_not_reject_legitimate_names() {
        for ok in [
            "fanqie-long-xianxia-outline",
            "我的写作技能",
            "My Skill 2",
            "skill_v2",
            "技能-第2版",
        ] {
            assert!(
                validated_skill_dir_name(ok).is_ok(),
                "🔴 合法技能名被误拒（用户会以为自己的技能包坏了）：{ok:?} → {:?}",
                validated_skill_dir_name(ok)
            );
        }
        // 前后空白应被裁掉而不是当成非法。
        assert_eq!(validated_skill_dir_name("  my-skill  ").unwrap(), "my-skill");
    }

    const LONG_FORM_SKILLS: &[&str] = &[
        "fanqie-long-xianxia-outline",
        "fanqie-long-xianxia-writer",
        "fanqie-long-xifang-outline",
        "fanqie-long-xifang-writer",
        "fanqie-long-lishi-outline",
        "fanqie-long-lishi-writer",
        "fanqie-long-youxi-outline",
        "fanqie-long-youxi-writer",
    ];

    fn resource_skills_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("skills")
    }

    #[test]
    fn long_form_skills_are_complete_builtin_resources() {
        let source_dir = resource_skills_dir();

        for skill_name in LONG_FORM_SKILLS {
            assert!(BUILTIN_SKILL_NAMES.contains(skill_name));
            let skill_dir = source_dir.join(skill_name);
            assert!(skill_dir.join("SKILL.md").is_file(), "{} 缺少 SKILL.md", skill_name);
            assert!(skill_dir.join("agents/openai.yaml").is_file(), "{} 缺少 agents/openai.yaml", skill_name);
            assert!(skill_dir.join("references").is_dir(), "{} 缺少 references 目录", skill_name);
        }
    }

    #[test]
    fn long_form_skills_can_be_discovered_and_copied_with_supporting_files() {
        let source_dir = resource_skills_dir();
        let mut discovered = Vec::new();
        collect_skills_from_root(&source_dir, &mut discovered);
        let discovered_names: Vec<&str> = discovered.iter().map(|skill| skill.name.as_str()).collect();

        for skill_name in LONG_FORM_SKILLS {
            assert!(discovered_names.contains(skill_name), "未发现 {}", skill_name);
        }

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("系统时间异常")
            .as_nanos();
        let temp = std::env::temp_dir().join(format!("museai-skill-copy-test-{}", unique));
        for skill_name in LONG_FORM_SKILLS {
            let destination = temp.join(skill_name);
            copy_dir_recursive(&source_dir.join(skill_name), &destination).expect("复制 Skill 失败");
            assert!(destination.join("SKILL.md").is_file());
            assert!(destination.join("agents/openai.yaml").is_file());
            assert!(WalkDir::new(destination.join("references"))
                .into_iter()
                .flatten()
                .any(|entry| entry.path().is_file()));
        }
        fs::remove_dir_all(temp).expect("清理临时目录失败");
    }
}
