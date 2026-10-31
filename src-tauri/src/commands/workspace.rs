use regex::Regex;
use std::sync::LazyLock;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

use crate::config::write_text_file;
use crate::openclaw_config::get_openclaw_dir;

/// Allowed workspace filenames (whitelist for security)
const ALLOWED_FILES: &[&str] = &[
    "AGENTS.md",
    "SOUL.md",
    "USER.md",
    "IDENTITY.md",
    "TOOLS.md",
    "MEMORY.md",
    "HEARTBEAT.md",
    "BOOTSTRAP.md",
    "BOOT.md",
];

fn validate_filename(filename: &str) -> Result<(), String> {
    if !ALLOWED_FILES.contains(&filename) {
        return Err(format!(
            "Invalid workspace filename: {filename}. Allowed: {}",
            ALLOWED_FILES.join(", ")
        ));
    }
    Ok(())
}

// --- Daily memory files (memory/YYYY-MM-DD.md) ---

static DAILY_MEMORY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}\.md$").unwrap());

fn validate_daily_memory_filename(filename: &str) -> Result<(), String> {
    if !DAILY_MEMORY_RE.is_match(filename) {
        return Err(format!(
            "Invalid daily memory filename: {filename}. Expected: YYYY-MM-DD.md"
        ));
    }
    Ok(())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyMemoryFileInfo {
    pub filename: String,
    pub date: String,
    pub size_bytes: u64,
    pub modified_at: u64,
    pub preview: String,
}

// --- Daily memory commands ---

/// List all daily memory files under `workspace/memory/`.
#[tauri::command]
pub async fn list_daily_memory_files() -> Result<Vec<DailyMemoryFileInfo>, String> {
    let memory_dir = get_openclaw_dir().join("workspace").join("memory");

    if !memory_dir.exists() {
        return Ok(Vec::new());
    }

    let mut files: Vec<DailyMemoryFileInfo> = Vec::new();

    let entries = std::fs::read_dir(&memory_dir)
        .map_err(|e| format!("Failed to read memory directory: {e}"))?;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }

        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }

        let date = name.trim_end_matches(".md").to_string();

        let size_bytes = meta.len();
        let modified_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let preview = std::fs::read_to_string(entry.path())
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>();

        files.push(DailyMemoryFileInfo {
            filename: name,
            date,
            size_bytes,
            modified_at,
            preview,
        });
    }

    // Sort by filename descending (newest date first, YYYY-MM-DD.md)
    files.sort_by(|a, b| b.filename.cmp(&a.filename));

    Ok(files)
}

/// Read a daily memory file.
#[tauri::command]
pub async fn read_daily_memory_file(filename: String) -> Result<Option<String>, String> {
    validate_daily_memory_filename(&filename)?;

    let path = get_openclaw_dir()
        .join("workspace")
        .join("memory")
        .join(&filename);

    if !path.exists() {
        return Ok(None);
    }

    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|e| format!("Failed to read daily memory file {filename}: {e}"))
}

/// Write a daily memory file (atomic write).
#[tauri::command]
pub async fn write_daily_memory_file(filename: String, content: String) -> Result<(), String> {
    validate_daily_memory_filename(&filename)?;

    let memory_dir = get_openclaw_dir().join("workspace").join("memory");

    std::fs::create_dir_all(&memory_dir)
        .map_err(|e| format!("Failed to create memory directory: {e}"))?;

    let path = memory_dir.join(&filename);

    write_text_file(&path, &content)
        .map_err(|e| format!("Failed to write daily memory file {filename}: {e}"))
}

/// Find the largest index `<= i` that is a valid UTF-8 char boundary.
/// Equivalent to the unstable `str::floor_char_boundary` (stabilized in 1.91).
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Find the smallest index `>= i` that is a valid UTF-8 char boundary.
/// Equivalent to the unstable `str::ceil_char_boundary` (stabilized in 1.91).
fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Search result for daily memory full-text search.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyMemorySearchResult {
    pub filename: String,
    pub date: String,
    pub size_bytes: u64,
    pub modified_at: u64,
    pub snippet: String,
    pub match_count: usize,
}

/// Full-text search across all daily memory files.
///
/// Performs case-insensitive search on both the date field and file content.
/// Returns results sorted by filename descending (newest first), each with a
/// snippet showing ~120 characters of context around the first match.
#[tauri::command]
pub async fn search_daily_memory_files(
    query: String,
) -> Result<Vec<DailyMemorySearchResult>, String> {
    let memory_dir = get_openclaw_dir().join("workspace").join("memory");

    if !memory_dir.exists() || query.is_empty() {
        return Ok(Vec::new());
    }

    let query_lower = query.to_lowercase();
    let mut results: Vec<DailyMemorySearchResult> = Vec::new();

    let entries = std::fs::read_dir(&memory_dir)
        .map_err(|e| format!("Failed to read memory directory: {e}"))?;

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };

        let date = name.trim_end_matches(".md").to_string();
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let content_lower = content.to_lowercase();

        // Count matches in content
        let content_matches: Vec<usize> = content_lower
            .match_indices(&query_lower)
            .map(|(i, _)| i)
            .collect();

        // Also check date field
        let date_matches = date.to_lowercase().contains(&query_lower);

        if content_matches.is_empty() && !date_matches {
            continue;
        }

        // Build snippet around first content match (~120 chars of context)
        let snippet = if let Some(&first_pos) = content_matches.first() {
            let start = if first_pos > 50 {
                floor_char_boundary(&content, first_pos - 50)
            } else {
                0
            };
            let end = ceil_char_boundary(&content, (first_pos + 70).min(content.len()));
            let mut s = String::new();
            if start > 0 {
                s.push_str("...");
            }
            s.push_str(&content[start..end]);
            if end < content.len() {
                s.push_str("...");
            }
            s
        } else {
            // Date-only match — use beginning of file as preview
            let end = ceil_char_boundary(&content, 120.min(content.len()));
            let mut s = content[..end].to_string();
            if end < content.len() {
                s.push_str("...");
            }
            s
        };

        let size_bytes = meta.len();
        let modified_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        results.push(DailyMemorySearchResult {
            filename: name,
            date,
            size_bytes,
            modified_at,
            snippet,
            match_count: content_matches.len(),
        });
    }

    // Sort by filename descending (newest date first)
    results.sort_by(|a, b| b.filename.cmp(&a.filename));

    Ok(results)
}

/// Delete a daily memory file (idempotent).
#[tauri::command]
pub async fn delete_daily_memory_file(filename: String) -> Result<(), String> {
    validate_daily_memory_filename(&filename)?;

    let path = get_openclaw_dir()
        .join("workspace")
        .join("memory")
        .join(&filename);

    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete daily memory file {filename}: {e}"))?;
    }

    Ok(())
}

// --- Workspace file commands ---

/// Read an OpenClaw workspace file content.
/// Returns None if the file does not exist.
#[tauri::command]
pub async fn read_workspace_file(filename: String) -> Result<Option<String>, String> {
    validate_filename(&filename)?;

    let path = get_openclaw_dir().join("workspace").join(&filename);

    if !path.exists() {
        return Ok(None);
    }

    std::fs::read_to_string(&path)
        .map(Some)
        .map_err(|e| format!("Failed to read workspace file {filename}: {e}"))
}

/// Write content to an OpenClaw workspace file (atomic write).
/// Creates the workspace directory if it does not exist.
#[tauri::command]
pub async fn write_workspace_file(filename: String, content: String) -> Result<(), String> {
    validate_filename(&filename)?;

    let workspace_dir = get_openclaw_dir().join("workspace");

    // Ensure workspace directory exists
    std::fs::create_dir_all(&workspace_dir)
        .map_err(|e| format!("Failed to create workspace directory: {e}"))?;

    let path = workspace_dir.join(&filename);

    write_text_file(&path, &content)
        .map_err(|e| format!("Failed to write workspace file {filename}: {e}"))
}

/// Open the workspace or memory directory in the system file manager.
/// `subdir`: "workspace" opens `~/.openclaw/workspace/`,
///           "memory" opens `~/.openclaw/workspace/memory/`.
#[tauri::command]
pub async fn open_workspace_directory(handle: AppHandle, subdir: String) -> Result<bool, String> {
    let dir = match subdir.as_str() {
        "memory" => get_openclaw_dir().join("workspace").join("memory"),
        _ => get_openclaw_dir().join("workspace"),
    };

    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {e}"))?;
    }

    handle
        .opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| format!("Failed to open directory: {e}"))?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在临时主目录里跑命令：`get_openclaw_dir` 经 `CC_SWITCH_TEST_HOME` 落到这里。
    /// 锁和 hermes/openclaw 的测试共用，避免并行测试互换临时主目录。
    ///
    /// 环境在整个 future 期间保持，await 完才还原。
    async fn with_test_home<T, F>(test: impl FnOnce() -> F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        // 异步侧用 `.lock().await`：guard 可 Send，不会持 std 锁跨 await。
        let _guard = crate::config::test_home_lock().lock().await;
        let temp = tempfile::tempdir().expect("create temp home");
        std::fs::create_dir_all(
            temp.path()
                .join(".openclaw")
                .join("workspace")
                .join("memory"),
        )
        .expect("create memory dir");
        let old_test_home = std::env::var_os("CC_SWITCH_TEST_HOME");
        let old_home = std::env::var_os("HOME");
        std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
        std::env::set_var("HOME", temp.path());
        let result = test().await;
        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match old_test_home {
            Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
            None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
        }
        result
    }

    // ---- 文件名白名单 ----

    #[test]
    fn workspace_filenames_are_whitelisted() {
        assert!(validate_filename("AGENTS.md").is_ok());
        assert!(validate_filename("MEMORY.md").is_ok());
        assert!(validate_filename("SOUL.md").is_ok());

        let message = validate_filename("agents.md").unwrap_err();
        assert!(message.contains("Invalid workspace filename"), "{message}");
        assert!(message.contains("AGENTS.md"), "{message}");

        // 路径穿越也过不了白名单。
        assert!(validate_filename("../AGENTS.md").is_err());
        assert!(validate_filename("AGENTS").is_err());
    }

    #[test]
    fn daily_memory_filenames_must_be_iso_dates() {
        assert!(validate_daily_memory_filename("2026-10-31.md").is_ok());

        for bad in [
            "2026-10-31.txt",
            "2026-10-3.md",
            "26-10-31.md",
            "2026-10-31",
            "../2026-10-31.md",
        ] {
            let message = validate_daily_memory_filename(bad).unwrap_err();
            assert!(
                message.contains("YYYY-MM-DD.md"),
                "unexpected message for {bad}: {message}"
            );
        }
    }

    #[test]
    fn char_boundary_helpers_round_to_utf8_boundaries() {
        // 两个三字节汉字加两个 ASCII：边界在 0/3/6/7/8。
        let text = "\u{4f60}\u{597d}ab";
        assert_eq!(text.len(), 8);
        assert_eq!(floor_char_boundary(text, 0), 0);
        assert_eq!(floor_char_boundary(text, 4), 3);
        assert_eq!(floor_char_boundary(text, 7), 7);
        assert_eq!(floor_char_boundary(text, 99), 8);
        assert_eq!(ceil_char_boundary(text, 4), 6);
        assert_eq!(ceil_char_boundary(text, 1), 3);
        assert_eq!(ceil_char_boundary(text, 8), 8);
        assert_eq!(ceil_char_boundary(text, 99), 8);
    }

    // ---- 命令 ----

    #[tokio::test]
    async fn daily_memory_write_read_list_delete_round_trip() {
        with_test_home(|| async {
            write_daily_memory_file("2026-10-01.md".into(), "older".into())
                .await
                .expect("write older");
            write_daily_memory_file("2026-10-31.md".into(), "newer".into())
                .await
                .expect("write newer");

            let read = read_daily_memory_file("2026-10-31.md".into())
                .await
                .expect("read");
            assert_eq!(read.as_deref(), Some("newer"));

            let listed = list_daily_memory_files().await.expect("list");
            assert_eq!(listed.len(), 2);
            // 新日期在前。
            assert_eq!(listed[0].filename, "2026-10-31.md");
            assert_eq!(listed[0].date, "2026-10-31");
            assert_eq!(listed[0].preview, "newer");
            assert_eq!(listed[1].filename, "2026-10-01.md");

            delete_daily_memory_file("2026-10-31.md".into())
                .await
                .expect("delete");
            assert_eq!(
                read_daily_memory_file("2026-10-31.md".into())
                    .await
                    .expect("read after delete"),
                None
            );
            // 幂等：再删一次不报错。
            delete_daily_memory_file("2026-10-31.md".into())
                .await
                .expect("delete twice");
        })
        .await;
    }

    #[tokio::test]
    async fn search_matches_content_and_dates_with_snippets() {
        with_test_home(|| async {
            // 匹配点在第 60 字节：片段要带前导省略号，且不越界。
            let mut content = "x".repeat(60);
            content.push_str("NEEDLE");
            content.push_str(&"y".repeat(60));
            write_daily_memory_file("2026-10-01.md".into(), content.clone())
                .await
                .expect("write content");
            write_daily_memory_file("2026-10-02.md".into(), "nothing here".into())
                .await
                .expect("write plain");

            let hits = search_daily_memory_files("needle".into())
                .await
                .expect("search");
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].filename, "2026-10-01.md");
            assert_eq!(hits[0].match_count, 1);
            assert!(hits[0].snippet.starts_with("..."), "{}", hits[0].snippet);
            assert!(hits[0].snippet.contains("NEEDLE"));

            // 日期也能命中：没有正文匹配时用文件开头兜底。
            let date_hits = search_daily_memory_files("2026-10-02".into())
                .await
                .expect("date search");
            assert_eq!(date_hits.len(), 1);
            assert_eq!(date_hits[0].filename, "2026-10-02.md");
            assert_eq!(date_hits[0].match_count, 0);
            assert_eq!(date_hits[0].snippet, "nothing here");

            // 空查询直接空结果。
            assert!(search_daily_memory_files(String::new())
                .await
                .expect("empty search")
                .is_empty());
        })
        .await;
    }

    #[tokio::test]
    async fn daily_memory_commands_refuse_unknown_filenames() {
        with_test_home(|| async {
            let message = write_daily_memory_file("../evil.md".into(), "x".into())
                .await
                .expect_err("traversal write");
            assert!(message.contains("YYYY-MM-DD.md"), "{message}");
            assert!(read_daily_memory_file("evil.md".into()).await.is_err());
            assert!(delete_daily_memory_file("evil.md".into()).await.is_err());
        })
        .await;
    }

    #[tokio::test]
    async fn list_and_search_are_empty_without_the_memory_dir() {
        with_test_home(|| async {
            let memory = get_openclaw_dir().join("workspace").join("memory");
            std::fs::remove_dir_all(&memory).expect("remove memory dir");
            assert!(list_daily_memory_files().await.expect("list").is_empty());
            assert!(search_daily_memory_files("anything".into())
                .await
                .expect("search")
                .is_empty());
        })
        .await;
    }
}
