use crate::error::AppError;
use chrono::Utc;
use std::fs;
use std::path::Path;

const MAX_BACKUPS: usize = 10;

/// 配置导入导出相关业务逻辑
pub struct ConfigService;

impl ConfigService {
    /// 为当前 config.json 创建备份，返回备份 ID（若文件不存在则返回空字符串）。
    pub fn create_backup(config_path: &Path) -> Result<String, AppError> {
        if !config_path.exists() {
            return Ok(String::new());
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_id = format!("backup_{timestamp}");

        let backup_dir = config_path
            .parent()
            .ok_or_else(|| AppError::Config("Invalid config path".into()))?
            .join("backups");

        fs::create_dir_all(&backup_dir).map_err(|e| AppError::io(&backup_dir, e))?;

        let backup_path = backup_dir.join(format!("{backup_id}.json"));
        let contents = fs::read(config_path).map_err(|e| AppError::io(config_path, e))?;
        fs::write(&backup_path, contents).map_err(|e| AppError::io(&backup_path, e))?;

        Self::cleanup_old_backups(&backup_dir, MAX_BACKUPS)?;

        Ok(backup_id)
    }

    fn cleanup_old_backups(backup_dir: &Path, retain: usize) -> Result<(), AppError> {
        if retain == 0 {
            return Ok(());
        }

        let entries = match fs::read_dir(backup_dir) {
            Ok(iter) => iter
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .map(|ext| ext == "json")
                        .unwrap_or(false)
                })
                .collect::<Vec<_>>(),
            Err(_) => return Ok(()),
        };

        if entries.len() <= retain {
            return Ok(());
        }

        let remove_count = entries.len().saturating_sub(retain);
        let mut sorted = entries;

        sorted.sort_by(|a, b| {
            let a_time = a.metadata().and_then(|m| m.modified()).ok();
            let b_time = b.metadata().and_then(|m| m.modified()).ok();
            a_time.cmp(&b_time)
        });

        for entry in sorted.into_iter().take(remove_count) {
            if let Err(err) = fs::remove_file(entry.path()) {
                log::warn!(
                    "Failed to remove old backup {}: {}",
                    entry.path().display(),
                    err
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cc-switch-config-test-{name}"));
        fs::create_dir_all(&dir).expect("create the test dir");
        dir
    }

    /// 给备份文件排一个明确的修改时间，轮转才能按年龄判定新旧。
    fn touch_with_age(path: &Path, age_secs: u64) {
        let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000 - age_secs);
        let file = fs::File::options()
            .write(true)
            .open(path)
            .expect("open the backup for its timestamp");
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .expect("set the modified time");
    }

    #[test]
    fn create_backup_reports_nothing_without_a_config() {
        let dir = temp_dir("no-config");
        let config = dir.join("config.json");

        assert_eq!(ConfigService::create_backup(&config).unwrap(), "");
        assert!(!dir.join("backups").exists(), "没有配置就不该建备份目录");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_backup_copies_the_config_under_a_timestamped_name() {
        let dir = temp_dir("one-backup");
        let config = dir.join("config.json");
        fs::write(&config, br#"{"version":2}"#).expect("write the config");

        let backup_id = ConfigService::create_backup(&config).expect("back up the config");
        assert!(
            backup_id.starts_with("backup_"),
            "unexpected backup id: {backup_id}"
        );

        let backup = dir.join("backups").join(format!("{backup_id}.json"));
        assert!(backup.is_file(), "备份没落盘: {}", backup.display());
        assert_eq!(fs::read(&backup).unwrap(), br#"{"version":2}"#);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_keeps_the_newest_backups_and_drops_the_oldest() {
        let dir = temp_dir("rotation");
        let backups = dir.join("backups");
        fs::create_dir_all(&backups).expect("create the backups dir");

        for index in 0..12 {
            let path = backups.join(format!("backup_{index:02}.json"));
            fs::write(&path, b"{}").expect("write a backup");
            touch_with_age(&path, 12 - index);
        }

        ConfigService::cleanup_old_backups(&backups, 10).expect("rotate the backups");

        let left: Vec<String> = fs::read_dir(&backups)
            .expect("read the backups dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left.len(), 10, "只保留 10 份");
        assert!(
            !left.contains(&"backup_00.json".to_string()),
            "最旧的两份该删"
        );
        assert!(!left.contains(&"backup_01.json".to_string()));
        assert!(
            left.contains(&"backup_11.json".to_string()),
            "最新的必须留下"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cleanup_leaves_foreign_files_and_zero_retention_alone() {
        let dir = temp_dir("foreign");
        let backups = dir.join("backups");
        fs::create_dir_all(&backups).expect("create the backups dir");

        let keep = backups.join("notes.txt");
        fs::write(&keep, b"keep me").expect("write a foreign file");
        let nested = backups.join("nested");
        fs::create_dir_all(&nested).expect("create a nested dir");

        ConfigService::cleanup_old_backups(&backups, 0).expect("a zero retention keeps everything");
        assert!(keep.is_file());
        assert!(nested.is_dir());

        for index in 0..3 {
            let path = backups.join(format!("backup_{index}.json"));
            fs::write(&path, b"{}").expect("write a backup");
        }
        ConfigService::cleanup_old_backups(&backups, 10).expect("fewer than the cap is a no-op");
        assert_eq!(
            fs::read_dir(&backups)
                .expect("read the backups dir")
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .count(),
            3
        );
        fs::remove_dir_all(&dir).ok();
    }
}
