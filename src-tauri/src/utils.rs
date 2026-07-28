use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use std::sync::OnceLock;
use tauri::{AppHandle, Manager, Runtime};
use walkdir::WalkDir;

use crate::models::*;

pub fn is_supported_content_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("md" | "txt" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

#[tauri::command]
pub fn read_file_with_lines(
    file_path: &str,
    offset: usize,
    limit: usize,
) -> Result<String, String> {
    let path = expand_path(None, file_path);
    if !path.exists() {
        return Err(format!("Error: {} not found", path.display()));
    }
    if !path.is_file() {
        return Err(format!(
            "Error: {} is a directory, not a file",
            path.display()
        ));
    }

    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok(String::from("(empty file)"));
    }

    let start = offset.saturating_sub(1);
    let limit = limit.clamp(1, MAX_READ_LINES);
    let chunk = lines.iter().skip(start).take(limit);
    let mut output = Vec::new();
    for (index, line) in chunk.enumerate() {
        output.push(format!("{}\t{}", start + index + 1, line));
    }
    if lines.len() > start + limit {
        output.push(format!(
            "... ({} lines total, showing {}-{})",
            lines.len(),
            start + 1,
            start + output.len()
        ));
    }

    Ok(output.join("\n"))
}

pub fn result_to_tool_result(result: Result<String, String>) -> ToolResult {
    match result {
        Ok(output) => ToolResult {
            success: true,
            output,
        },
        Err(output) => ToolResult {
            success: false,
            output,
        },
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn resolve_document_dir_with_fallback(
    system_result: Result<PathBuf, String>,
    home: Option<PathBuf>,
    allow_linux_fallback: bool,
) -> Result<PathBuf, String> {
    match system_result {
        Ok(path) => Ok(path),
        Err(error) => {
            if allow_linux_fallback {
                if let Some(home) = home.filter(|path| !path.as_os_str().is_empty()) {
                    return Ok(home.join("Documents"));
                }
            }
            Err(error)
        }
    }
}

pub fn resolve_document_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    resolve_document_dir_with_fallback(
        app.path().document_dir().map_err(|error| error.to_string()),
        env::var_os("HOME").map(PathBuf::from),
        cfg!(target_os = "linux"),
    )
}

pub fn expand_path(base_dir: Option<&str>, path: &str) -> std::path::PathBuf {
    if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    if let Some(stripped) = path.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(stripped);
        }
    }
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    if let Some(base) = base_dir {
        if !base.trim().is_empty() {
            return Path::new(base).join(p);
        }
    }
    std::env::current_dir().unwrap_or_default().join(p)
}

pub fn normalize_tool_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 🔴 **全仓唯一的「这个字符串能不能当一个路径分量用」判据。**
///
/// 凡是把外部来的名字 `join` 进路径的地方都要先过这里。不过的后果一律是路径穿越：
/// 写出目标目录、读到目录外的文件、或 `remove_*` 删掉不该删的东西。
/// ⚠️ 绝对路径尤其致命——**Rust 的 `Path::join` 遇到绝对路径会整个替换基路径**，连 `..` 都不需要。
///
/// 判据取「必须是单个、普通的路径分量」：非空、不含分隔符与 NUL、不是 `.`/`..`、
/// 不以 `.` 开头（避免写出隐藏目录）、必须恰好一个分量。
///
/// **刻意不限制字符集**：技能名可以是中文、版本备注可以带空格；用白名单字符集会把合法输入
/// 挡在外面，而挡住穿越并不需要那么严。这一取舍由各调用点的「合法名不得被误拒」用例钉住。
///
/// ⚠️ 「必须恰好一个分量」这一条在 Unix/Windows 上几乎冗余（绝对路径必含分隔符，
/// 已被上一条拦下）；留着是因为它表达的是**意图**而非手段。故障注入实测它不承重，
/// 如实记在这里，免得有人以为每一条都被验证过。
pub fn validated_path_component<'a>(value: &'a str, what: &str) -> Result<&'a str, String> {
    let v = value.trim();
    if v.is_empty() {
        return Err(format!("{what}不能为空"));
    }
    if v == "." || v == ".." || v.starts_with('.') {
        return Err(format!("{what}非法（不得以 . 开头或为 . / ..）：{value}"));
    }
    if v.contains('/') || v.contains('\\') || v.contains('\0') {
        return Err(format!("{what}非法（不得含路径分隔符）：{value}"));
    }
    if Path::new(v).is_absolute() || Path::new(v).components().count() != 1 {
        return Err(format!("{what}必须是单个名称：{value}"));
    }
    Ok(v)
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::Prefix(p) => {
                result.push(p.as_os_str());
                if cfg!(target_os = "windows") {
                    match p.kind() {
                        std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_) => {
                            result.push("\\");
                        }
                        _ => {}
                    }
                }
            }
            std::path::Component::RootDir => {
                if !cfg!(target_os = "windows") || result.as_os_str().is_empty() {
                    result.push(std::path::MAIN_SEPARATOR_STR);
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::Normal(name) => result.push(name),
        }
    }
    result
}

pub fn count_lines(content: &str) -> usize {
    content.matches('\n').count() + usize::from(!content.is_empty() && !content.ends_with('\n'))
}

pub fn simple_diff(old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut lines = vec![String::from("--- before"), String::from("+++ after")];
    for line in old_lines
        .iter()
        .filter(|line| !new_lines.contains(line))
        .take(20)
    {
        lines.push(format!("-{}", line));
    }
    for line in new_lines
        .iter()
        .filter(|line| !old_lines.contains(line))
        .take(20)
    {
        lines.push(format!("+{}", line));
    }
    lines.join("\n")
}

pub fn truncate_middle(content: String, max_chars: usize) -> String {
    let char_count = content.chars().count();
    if char_count <= max_chars {
        return content;
    }

    let head: String = content.chars().take(6000).collect();
    let tail: String = content
        .chars()
        .rev()
        .take(3000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{}\n\n... truncated ({} chars total) ...\n\n{}",
        head, char_count, tail
    )
}
pub fn truncate_chars(content: &str, max_chars: usize) -> String {
    if content.chars().count() <= max_chars {
        content.to_string()
    } else {
        format!(
            "{}\n... (truncated)",
            content.chars().take(max_chars).collect::<String>()
        )
    }
}
pub fn collect_grep_files(
    base: &Path,
    include: Option<&str>,
) -> Result<Vec<std::path::PathBuf>, String> {
    if base.is_file() {
        return Ok(vec![base.to_path_buf()]);
    }
    if !base.is_dir() {
        return Err(format!("Error: {} is not a directory", base.display()));
    }

    let mut files = Vec::new();
    for entry in WalkDir::new(base).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        !matches!(
            name.as_ref(),
            ".git"
                | "node_modules"
                | "__pycache__"
                | ".venv"
                | "venv"
                | ".tox"
                | "dist"
                | "build"
                | "target"
        )
    }) {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(include) = include {
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !glob_match(include, file_name) {
                continue;
            }
        }
        files.push(path.to_path_buf());
        if files.len() >= 5000 {
            break;
        }
    }
    Ok(files)
}
pub fn glob_match(pattern: &str, file_name: &str) -> bool {
    let temp = env::temp_dir().join(file_name);
    glob::Pattern::new(pattern)
        .map(|pattern| pattern.matches_path(&temp) || pattern.matches(file_name))
        .unwrap_or(false)
}
static PYTHON_INFO: OnceLock<String> = OnceLock::new();

pub fn get_python_info() -> &'static str {
    PYTHON_INFO.get_or_init(|| {
        let output = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .or_else(|_| {
                std::process::Command::new("python")
                    .arg("--version")
                    .output()
            });

        let version = match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if stdout.is_empty() {
                    String::from_utf8_lossy(&out.stderr).trim().to_string()
                } else {
                    stdout
                }
            }
            _ => String::from("未检测到 Python 环境"),
        };

        let path_output = if cfg!(target_os = "windows") {
            std::process::Command::new("where").arg("python").output()
        } else {
            std::process::Command::new("which")
                .arg("python3")
                .output()
                .or_else(|_| std::process::Command::new("which").arg("python").output())
        };

        let path = match path_output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => String::from("未知"),
        };

        if version.contains("未检测到") {
            version
        } else {
            format!("{} ({})", version, path)
        }
    })
}
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.map_err(std::io::Error::other)?;
        let ty = entry.file_type();
        let dest_path = dst.join(entry.path().strip_prefix(src).unwrap());
        if ty.is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}
pub fn copy_md_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in walkdir::WalkDir::new(src) {
        let entry = entry.map_err(std::io::Error::other)?;
        if entry
            .path()
            .components()
            .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }

        let ty = entry.file_type();
        let relative_path = entry.path().strip_prefix(src).unwrap();
        if relative_path.as_os_str().is_empty() {
            continue;
        }

        let dest_path = dst.join(relative_path);
        if ty.is_file()
            && is_supported_content_file(entry.path()) {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(entry.path(), &dest_path)?;
            }
    }
    Ok(())
}
pub fn extract_frontmatter_value(body: &str, key: &str) -> Option<String> {
    body.lines().find_map(|line| {
        let (left, right) = line.split_once(':')?;
        if left.trim() == key {
            Some(right.trim().trim_matches('"').to_string())
        } else {
            None
        }
    })
}
pub fn now_millis() -> Result<u64, String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_millis();
    u64::try_from(millis).map_err(|_| String::from("当前时间戳过大"))
}

/// 🔴 **全仓有三份「名字能不能当路径用」的校验，它们的严格程度不同，且不该被合并。**
///
/// | 判据 | 用于 | 严格度 | 为什么是这个严格度 |
/// |---|---|---|---|
/// | [`validated_path_component`] | 技能名、版本 id | 允许任意字符集，只挡穿越 | 技能名可以是中文、版本备注可以带空格——白名单字符集会把合法输入挡在外面 |
/// | [`sanitize_session_id`]（本函数） | 会话 id | ASCII 字母数字 + `-` `_` | 会话 id 是**机器生成**的（`partner-session-<uuid>`），没有任何理由出现别的字符；这里可以也应该更严 |
/// | `crawler::sanitize_filename` | 抓来的书名/章节名 | **不拒绝，改写** | 它面对的是远端页面标题，拒绝等于抓取失败；正确处理是把非法字符换成全角、把 `.`/`..`/空名换成安全值 |
///
/// ⚠️ **别把它们合并成一个。** 用最严的那份去校验技能名会拒掉中文技能包；
/// 用最松的那份去校验会话 id 会白白放宽一个本可以很严的地方；
/// 而把 crawler 那份改成「拒绝」会让一个正常书名的抓取任务直接失败。
/// 三者的差异不是历史遗留，是**输入来源不同**（人写的名字 / 机器生成的 id / 远端内容）。
pub fn sanitize_session_id(id: &str) -> Result<String, String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err(String::from("session id 不能为空"));
    }
    if !trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(String::from("session id 包含非法字符"));
    }
    Ok(trimmed.to_string())
}
pub fn agent_sessions_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let doc_dir = resolve_document_dir(app)?;
    Ok(doc_dir.join("MuseAI").join("agent-sessions"))
}

pub fn agent_session_path(app: &AppHandle, id: &str) -> Result<std::path::PathBuf, String> {
    let safe_id = sanitize_session_id(id)?;
    Ok(agent_sessions_dir(app)?.join(format!("{}.json", safe_id)))
}

// ═══════════════════════════════════════════════════════════════════════════
// 覆盖既有用户数据的落盘，必须是原子的
// ═══════════════════════════════════════════════════════════════════════════

/// 原子落盘：写同目录临时文件 → `rename` 覆盖目标。用于**覆盖已有用户数据**的每一处写。
///
/// # 它替掉的是什么
///
/// `fs::write` 是「**先把目标截成 0 字节，再往里写**」。中途进程没了（崩溃、Cmd-Q、
/// force quit、被 kill、OOM），留下的就是一个**截断或全空**的文件。对这个仓库最要命的是
/// `config/partner-store.json`（全部角色卡 + 世界书）和 `config/settings-store.json`
/// （全部模型配置、API Key、每一条自定义 prompt）——它们由 zustand persist 在**每次状态变更时
/// 落盘**，也就是正常使用中一直在写，窗口不是理论上的。
///
/// 🔴 而且损坏会**自我固化**：下次启动时截断的 JSON 解析失败 → persist 静默回落到初始状态 →
/// 用户随手一改，第一次 `setItem` 就把那份空状态原样写回文件。原始内容至此彻底没有第二份。
///
/// `rename` 覆盖是原子的：任何时刻别的进程看到的要么是**完整的旧内容**、要么是**完整的新内容**，
/// 没有中间态。临时文件与目标**同目录**（跨文件系统 rename 会失败），失败路径一律清掉它。
///
/// # 边界：不做 fsync，这是有意的
///
/// 本函数消掉的是**进程死亡**窗口，不是**断电 / 内核 panic** 窗口——后者需要在 rename 前
/// `sync_all()`，而那在 macOS 上是 `F_FULLFSYNC`（整盘刷），按「每次状态变更一次」的写入频率
/// 摊到设置页的每一次输入上，会变成肉眼可见的卡顿。用一次真实的交互退化去换一个由
/// 文件系统本身已经大幅缓解的窗口（APFS 与 ext4 `data=ordered` 都对「写完即 rename 覆盖」
/// 有强制落盘的启发式），不划算。
///
/// # 临时文件名
///
/// `<目标文件名>.tmp-<pid>-<序号>`：
/// - 后缀**不是 `.json`**，因为会话目录与配置目录的枚举器都按 `extension() == "json"` 过滤
///   （`sessions.rs` / `workspace.rs`）——残留的临时文件不会被当成一条坏会话列出来。
/// - 带 pid + 进程内递增序号：桌面端与手机端可能同时保存同一个 store，共用一个固定临时名
///   会让两次写交叉后 rename 出一份**混合内容**。序号用 `AtomicU64` 而非随机数/时间戳，
///   与 `docs/VALIDATION.md` §4「禁三样」同口径。
pub fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    let dir = path
        .parent()
        .ok_or_else(|| format!("无法定位 {} 的父目录", path.display()))?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("目标文件名不可用：{}", path.display()))?;
    let tmp = dir.join(format!(
        "{}.tmp-{}-{}",
        stem,
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let written = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(content.as_bytes())
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("museai-utils-{}-{}", name, nanos));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// 覆盖写的基本契约：内容换掉了，且**不留临时文件**。
    ///
    /// 残留检查不是洁癖：`config/` 与 `agent-sessions/` 都会被枚举，攒一地临时文件既是
    /// 磁盘泄漏、也可能被将来的枚举器当成真文件读出来。
    #[test]
    fn write_atomic_overwrites_and_leaves_no_residue() {
        let dir = temp_dir("atomic-overwrite");
        let path = dir.join("partner-store.json");

        super::write_atomic(&path, r#"{"cards":["旧"]}"#).expect("首次写入");
        super::write_atomic(&path, r#"{"cards":["新","新2"]}"#).expect("覆盖写入");

        assert_eq!(
            std::fs::read_to_string(&path).expect("读回"),
            r#"{"cards":["新","新2"]}"#
        );
        let left: Vec<String> = std::fs::read_dir(&dir)
            .expect("列目录")
            .map(|e| e.expect("目录项").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["partner-store.json".to_string()], "🔴 目录里多了东西：{left:?}");

        // 父目录不存在时自建（`save_app_state` 首次运行就是这个情形）。
        let nested = dir.join("config").join("settings-store.json");
        super::write_atomic(&nested, "{}").expect("应自建父目录");
        assert_eq!(std::fs::read_to_string(&nested).expect("读回"), "{}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **rename 失败时不许留下临时文件**（`docs/VALIDATION.md` §3.47 欠账 A2）。
    ///
    /// 登记这条欠账时我写的是「测试里造不出『文件写成了但 rename 失败』的情形
    /// （需要跨文件系统或权限构造）」。**那句话没走一遍就写了**——
    /// 其实有一个再简单不过的构造：**让目标路径是一个非空目录**。
    /// 临时文件会正常写成，`fs::rename` 到目录上必然失败（EISDIR/ENOTEMPTY）。
    ///
    /// 这条路径若不清理，`config/` 与 `agent-sessions/` 会随每次失败攒下垃圾文件，
    /// 而失败本身是静默的（`write_atomic` 只把错误往上抛，调用方多半只 log）。
    ///
    /// ⚠️ `#[cfg(unix)]`：Windows 上 `MoveFileEx` 对「目标是目录」的行为与 Unix 不同，
    /// 那是**构造手法**的平台差异，不是被测行为的差异。
    #[cfg(unix)]
    #[test]
    fn write_atomic_cleans_up_its_temp_file_when_the_rename_fails() {
        let dir = temp_dir("atomic-rename-fail");
        // 目标路径是一个**非空目录** → rename 必然失败。
        let target = dir.join("target.json");
        std::fs::create_dir_all(&target).expect("把目标建成目录");
        std::fs::write(target.join("occupant"), "x").expect("让它非空");

        let err = super::write_atomic(&target, "新内容").expect_err("rename 到非空目录必须失败");
        assert!(!err.is_empty(), "错误信息不该是空的：{err:?}");

        // 🔴 关键断言：目录里除了那个目标目录，不许多出任何 `.tmp-*` 残留。
        let left: Vec<String> = std::fs::read_dir(&dir)
            .expect("列目录")
            .map(|e| e.expect("目录项").file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            vec!["target.json".to_string()],
            "🔴 rename 失败后临时文件没被清掉，`config/` 会随每次失败攒垃圾：{left:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔴 **本函数存在的全部理由**：目标文件从头到尾没有被原地改过。
    ///
    /// `fs::write` 是「先把目标截成 0 再写」——进程在这中间死掉（崩溃 / Cmd-Q / kill / OOM），
    /// 留下的就是一个截断文件。这里用 inode 与**改动前就打开的句柄**同时验证：
    /// 新内容是 `rename` 换上去的，旧 inode 一个字节都没动。测试杀不掉自己来演示崩溃，
    /// 但「旧内容从未被就地覆盖」正是崩溃窗口不存在的**结构性**理由。
    ///
    /// ⚠️ `#[cfg(unix)]`：Windows 上 `File::open` 不带 `FILE_SHARE_DELETE`，
    /// 对一个已打开的文件做 rename 覆盖会失败——那是**测试手法**的平台差异，
    /// 不是被测行为的差异（`fs::rename` 在 Windows 上走
    /// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)`，同样是原子替换）。
    #[cfg(unix)]
    #[test]
    fn write_atomic_never_overwrites_the_old_file_in_place() {
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;

        let dir = temp_dir("atomic-inode");
        let path = dir.join("settings-store.json");
        super::write_atomic(&path, "旧内容").expect("首次写入");

        let before = std::fs::metadata(&path).expect("元信息").ino();
        let mut held = std::fs::File::open(&path).expect("改动前先握住旧文件");

        super::write_atomic(&path, "新内容").expect("覆盖写入");

        let mut old = String::new();
        held.read_to_string(&mut old).expect("从旧句柄读");
        assert_eq!(old, "旧内容", "🔴 旧文件被原地改了 —— 截断窗口还在");
        assert_ne!(
            before,
            std::fs::metadata(&path).expect("元信息").ino(),
            "🔴 目标仍是同一个 inode —— 说明没走 rename，是原地写"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("读回"), "新内容");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 递归收集 `src-tauri/src` 下的生产源码（花括号配平剥掉 `#[cfg(test)] mod X { .. }`）。
    ///
    /// 与 `server/src/testkit.rs::production_sources` 同一套判据。本 crate 目前 16 个内联
    /// 测试模块**全部叫 `tests`**，但仍按配平剥离而不是按名字截断——一旦将来出现
    /// 文件中段的测试夹具（server 侧就有），按名字截断会把其后的生产码整段漏掉，
    /// 让扫描形同虚设，而且**不会有任何报错**。
    #[cfg(test)]
    fn production_sources() -> Vec<(String, String)> {
        fn strip_test_mods(src: &str) -> String {
            let mut out = String::with_capacity(src.len());
            let bytes = src.as_bytes();
            let mut i = 0usize;
            while i < src.len() {
                let Some(rel) = src[i..].find("#[cfg(test)]") else {
                    out.push_str(&src[i..]);
                    break;
                };
                let marker = i + rel;
                let after = marker + "#[cfg(test)]".len();
                let rest = &src[after..];
                let pad = rest.len() - rest.trim_start().len();
                let head = rest.trim_start();
                let is_mod_block = head.starts_with("mod ")
                    && head[..head.find(['{', ';']).map(|k| k + 1).unwrap_or(head.len())]
                        .ends_with('{');
                if !is_mod_block {
                    out.push_str(&src[i..after]);
                    i = after;
                    continue;
                }
                out.push_str(&src[i..marker]);
                let brace_at = after + pad + head.find('{').expect("上面已确认有 {");
                let mut depth = 0i32;
                let mut j = brace_at;
                while j < bytes.len() {
                    match bytes[j] {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                j += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                i = j;
            }
            out
        }
        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
            let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
                .unwrap_or_else(|e| panic!("读目录 {dir:?}：{e}"))
                .map(|e| e.expect("目录项").path())
                .collect();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    walk(&path, root, out);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {path:?}：{e}"));
                let rel = path
                    .strip_prefix(root)
                    .expect("相对路径")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, strip_test_mods(&src)));
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        walk(&root, &root, &mut out);
        assert!(out.len() > 15, "🔴 源码遍历只收到 {} 个文件，扫描口径坏了", out.len());
        out
    }

    /// 🔴 **覆盖既有用户数据的落盘，一处都不许用 `fs::write`。**
    ///
    /// # 为什么是「排除表」而不是「清单」
    ///
    /// 这道门要防的是**将来新写的落盘点忘了走原子写**。若写成「以下这些点必须原子」，
    /// 新增的第 N+1 处不在清单里 → 静默放行，门等于不存在。反过来，「除了这几处已知安全的，
    /// 一律不许出现 `fs::write`」让**遗漏往红的方向失败**：新写一处就红，作者必须在
    /// 「改成 `write_atomic`」和「在这张表里写清为什么安全」之间选一个。
    /// 判据方向的选法见 `docs/VALIDATION.md` §3.8.1。
    ///
    /// # 表里为什么按「处数」而不是只按文件
    ///
    /// 只记文件名的话，往一个已豁免文件里新加一处 `fs::write` 会被顺带放行。
    /// 记数字，加一处就对不上。
    #[test]
    fn red_line_every_write_that_overwrites_user_data_is_atomic() {
        // (相对 src/ 的路径, 该文件允许的 `fs::write(` 处数, 为什么这几处安全)
        const EXEMPT: &[(&str, usize, &str)] = &[
            (
                "crawler.rs",
                4,
                "抓下来的网页正文/目录：写的是新建文件，且内容随时可以重抓，不是不可再生的用户数据",
            ),
            (
                "fs_commands.rs",
                1,
                "新建空文件（`fs::write(&item_path, \"\")`）：目标此前不存在，没有任何内容会被截断",
            ),
            (
                "agent/sessions.rs",
                1,
                "反向大纲保存：路径来自 `unique_reverse_outline_path`，它逐个试到不存在的文件名才返回，写的一定是新文件",
            ),
            (
                "commands/fs.rs",
                3,
                "两处导出到下载目录（路径来自 `unique_download_path`，同样只返回不存在的路径）+ 一处 `create_file`（已先 `if path.exists() { return Err }`）",
            ),
        ];

        let mut offenders = Vec::new();
        for (rel, src) in production_sources() {
            let found = src.matches("fs::write(").count();
            let allowed = EXEMPT
                .iter()
                .find(|(p, _, _)| *p == rel)
                .map(|(_, n, _)| *n)
                .unwrap_or(0);
            if found != allowed {
                offenders.push(format!("{rel}：找到 {found} 处 `fs::write(`，表里登记 {allowed} 处"));
            }
        }
        assert!(
            offenders.is_empty(),
            "🔴 落盘点与豁免表对不上：\n  {}\n\n\
             覆盖既有用户数据的写必须走 `utils::write_atomic`（写同目录临时文件 → rename）。\
             `fs::write` 先把目标截成 0 再写，进程在这中间死掉就留下一个截断文件——\
             对 `config/partner-store.json`（全部角色卡）和 `config/settings-store.json`\
             （全部模型配置与 API Key）而言那就是数据没了，而且下次启动会被空状态覆盖固化。\n\
             确实安全（写的是新建文件 / 空文件 / 可再生内容）就把它加进本用例的 EXEMPT 表并写清理由。",
            offenders.join("\n  ")
        );

        // 表本身别烂掉：登记了却已经不存在的文件要及时删掉，否则它会悄悄放行一个同名新文件。
        let present: std::collections::BTreeSet<String> =
            production_sources().into_iter().map(|(rel, _)| rel).collect();
        for (rel, _, _) in EXEMPT {
            assert!(present.contains(*rel), "🔴 EXEMPT 里登记的 {rel} 已不在源码树里，请删掉这一行");
        }
    }

    /// 🔴 共用判据本身：三个调用点（技能导入 / 技能删除 / 版本文件）都靠它。
    #[test]
    fn validated_path_component_rejects_traversal_and_keeps_normal_names() {
        for bad in ["../x", "a/b", "a\\b", "/abs", "..", ".", ".hidden", "", "   "] {
            assert!(
                super::validated_path_component(bad, "名称").is_err(),
                "🔴 应被拒：{bad:?}"
            );
        }
        for ok in ["fanqie-long-xianxia-outline", "我的技能", "draft 2", "v1_2"] {
            assert!(
                super::validated_path_component(ok, "名称").is_ok(),
                "🔴 合法名不得被误拒：{ok:?}"
            );
        }
        assert_eq!(super::validated_path_component("  x  ", "名称").unwrap(), "x");
    }

    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_is_supported_content_file() {
        assert!(is_supported_content_file(Path::new("article.md")));
        assert!(is_supported_content_file(Path::new("notes.txt")));
        assert!(is_supported_content_file(Path::new("image.PNG")));
        assert!(is_supported_content_file(Path::new("photo.jpg")));
        assert!(!is_supported_content_file(Path::new("script.py")));
        assert!(!is_supported_content_file(Path::new("data.json")));
        assert!(!is_supported_content_file(Path::new("no_extension")));
    }

    #[test]
    fn test_expand_path_home() {
        let home = home_dir().expect("home dir should be resolvable");
        let result = expand_path(None, "~");
        assert_eq!(result, home);

        let result = expand_path(None, "~/Documents");
        assert_eq!(result, home.join("Documents"));
    }

    #[test]
    fn test_expand_path_absolute() {
        if cfg!(target_os = "windows") {
            let result = expand_path(None, "C:\\Program Files\\app");
            assert_eq!(result, PathBuf::from("C:\\Program Files\\app"));
        } else {
            let result = expand_path(None, "/usr/local/bin");
            assert_eq!(result, PathBuf::from("/usr/local/bin"));
        }
    }

    #[test]
    fn test_expand_path_relative_with_base() {
        if cfg!(target_os = "windows") {
            let result = expand_path(Some("C:\\base"), "subdir\\file.txt");
            assert_eq!(result, PathBuf::from("C:\\base\\subdir\\file.txt"));
        } else {
            let result = expand_path(Some("/base"), "subdir/file.txt");
            assert_eq!(result, PathBuf::from("/base/subdir/file.txt"));
        }
    }

    #[test]
    fn test_resolve_document_dir_prefers_system_path() {
        let system_dir = PathBuf::from("/custom/documents");
        let result = resolve_document_dir_with_fallback(
            Ok(system_dir.clone()),
            Some(PathBuf::from("/home/test")),
            true,
        );

        assert_eq!(result, Ok(system_dir));
    }

    #[test]
    fn test_resolve_document_dir_falls_back_on_linux() {
        let result = resolve_document_dir_with_fallback(
            Err("unknown path".to_string()),
            Some(PathBuf::from("/home/test")),
            true,
        );

        assert_eq!(result, Ok(PathBuf::from("/home/test/Documents")));
    }

    #[test]
    fn test_resolve_document_dir_rejects_empty_linux_home() {
        let result = resolve_document_dir_with_fallback(
            Err("unknown path".to_string()),
            Some(PathBuf::new()),
            true,
        );

        assert_eq!(result, Err("unknown path".to_string()));
    }

    #[test]
    fn test_resolve_document_dir_does_not_fallback_off_linux() {
        let result = resolve_document_dir_with_fallback(
            Err("unknown path".to_string()),
            Some(PathBuf::from("/home/test")),
            false,
        );

        assert_eq!(result, Err("unknown path".to_string()));
    }

    #[test]
    fn test_normalize_tool_path() {
        let path = Path::new("dir\\subdir\\file.txt");
        assert_eq!(normalize_tool_path(path), "dir/subdir/file.txt");
    }

    #[test]
    fn test_normalize_path() {
        let path = Path::new("/a/b/../c");
        assert_eq!(normalize_path(path), PathBuf::from("/a/c"));

        let path = Path::new("/a/./b/c");
        assert_eq!(normalize_path(path), PathBuf::from("/a/b/c"));
    }

    #[test]
    fn test_count_lines() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("single"), 1);
        assert_eq!(count_lines("line1\nline2\nline3"), 3);
        assert_eq!(count_lines("line1\nline2\n"), 2);
    }

    #[test]
    fn test_simple_diff() {
        let old = "apple\nbanana";
        let new = "apple\ncherry";
        let diff = simple_diff(old, new);
        assert!(diff.contains("--- before"));
        assert!(diff.contains("+++ after"));
        assert!(diff.contains("-banana"));
        assert!(diff.contains("+cherry"));
    }

    #[test]
    fn test_truncate_chars_short() {
        let text = "short";
        assert_eq!(truncate_chars(text, 100), "short");
    }

    #[test]
    fn test_truncate_chars_long() {
        let text = "a".repeat(200);
        let result = truncate_chars(&text, 100);
        assert!(result.starts_with("a".repeat(100).as_str()));
        assert!(result.contains("... (truncated)"));
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.md", "file.md"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.md", "file.txt"));
        assert!(glob_match("test_*", "test_something"));
    }

    #[test]
    fn test_extract_frontmatter_value() {
        let body = r#"name: "Test Skill"
description: "A test skill"
version: "1.0.0"
"#;
        assert_eq!(
            extract_frontmatter_value(body, "name"),
            Some("Test Skill".to_string())
        );
        assert_eq!(
            extract_frontmatter_value(body, "version"),
            Some("1.0.0".to_string())
        );
        assert_eq!(extract_frontmatter_value(body, "missing"), None);
    }

    #[test]
    fn test_sanitize_session_id_valid() {
        assert_eq!(sanitize_session_id("session-123").unwrap(), "session-123");
        assert_eq!(
            sanitize_session_id("  session_456  ").unwrap(),
            "session_456"
        );
    }

    #[test]
    fn test_sanitize_session_id_invalid() {
        assert!(sanitize_session_id("").is_err());
        assert!(sanitize_session_id("   ").is_err());
        assert!(sanitize_session_id("session@123").is_err());
        assert!(sanitize_session_id("session 123").is_err());
    }
}
