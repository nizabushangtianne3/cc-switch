//! Claude Code 存登录的地方（「槽位」）：读、写、删整包凭据。
//!
//! - 默认槽位是 `~/.claude` 对应的那一个；账号槽位是 `<数据目录>/claude-accounts/<id>/config`。
//! - macOS：钥匙串 generic password，service 见 [`Slot::keychain_service`]，内容是凭据 JSON；
//! - 其他平台：槽位目录里的 `.credentials.json`（0600）。
//!
//! 凭据只在内存里经过（[`Secret`] 不实现可打印的 `Debug`），不落到 CC Switch 自己的任何文件。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::error::AppError;
use crate::services::claude_account::layout;

/// 一个槽位。
#[derive(Clone, PartialEq, Eq)]
pub enum Slot {
    /// 用户的 `~/.claude`：不设 `CLAUDE_CONFIG_DIR` 时 Claude Code 用的那一个（`home` 是用户目录）
    Default { home: PathBuf },
    /// 某个账号目录（`CLAUDE_CONFIG_DIR` 的原字符串）
    Account(PathBuf),
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Slot::Default { home } => write!(f, "Slot::Default({})", home.display()),
            Slot::Account(dir) => write!(f, "Slot::Account({})", dir.display()),
        }
    }
}

impl Slot {
    /// 本机用户的默认槽位。
    pub fn default_slot() -> Slot {
        Slot::Default {
            home: crate::config::get_home_dir(),
        }
    }

    /// 放凭据文件和锁的目录。
    pub fn secure_dir(&self) -> PathBuf {
        match self {
            Slot::Default { home } => home.join(".claude"),
            Slot::Account(dir) => dir.clone(),
        }
    }

    /// 这个槽位的 `.claude.json`（账号资料 `oauthAccount` 在这里）。和 Claude Code 一样：
    /// 目录里有非空的旧版 `.config.json` 时用它。
    pub fn global_config(&self) -> PathBuf {
        let legacy = self.secure_dir().join(".config.json");
        if std::fs::metadata(&legacy).is_ok_and(|meta| meta.is_file() && meta.len() > 0) {
            return legacy;
        }
        match self {
            Slot::Default { home } => home.join(".claude.json"),
            Slot::Account(dir) => dir.join(".claude.json"),
        }
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.secure_dir().join(".credentials.json")
    }

    /// macOS 钥匙串的 service：默认槽位没有后缀，账号目录按 Claude Code 的规则加后缀。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn keychain_service(&self) -> String {
        match self {
            Slot::Default { .. } => "Claude Code-credentials".to_string(),
            Slot::Account(dir) => layout::keychain_service(dir),
        }
    }
}

/// 整包凭据。不能打印，比较用 [`Secret::fingerprint`]。
#[derive(Clone, PartialEq)]
pub struct Secret(pub Map<String, Value>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&String> = self.0.keys().collect();
        write!(f, "Secret(keys={keys:?})")
    }
}

impl Secret {
    pub fn parse(bytes: &[u8]) -> Result<Secret, AppError> {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(Value::Object(map)) => Ok(Secret(map)),
            // 不把解析错误原文带出去：里面可能夹着内容片段
            _ => Err(AppError::localized(
                "claude.snapshot.unreadable",
                "Claude Code 的登录信息格式无法识别，没有做任何改动",
                "Claude Code's sign-in data has an unrecognized format. Nothing was changed.",
            )),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&Value::Object(self.0.clone())).unwrap_or_default()
    }

    /// 内容的 sha256（只用来比较前后是不是同一份，单向、不可还原）。
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.to_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// 读、写、删一个槽位的整包凭据。测试里换成内存实现。
pub trait CredentialStore: Send + Sync {
    /// 没有登录时为 `None`。
    fn read(&self, slot: &Slot) -> Result<Option<Secret>, AppError>;
    fn write(&self, slot: &Slot, secret: &Secret) -> Result<(), AppError>;
    /// 不在时也算成功。
    fn delete(&self, slot: &Slot) -> Result<(), AppError>;
}

/// 本平台 Claude Code 用的那种存法。
pub fn platform_store() -> Box<dyn CredentialStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(keychain::KeychainStore)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(FileStore)
    }
}

/// `.credentials.json`（Linux / Windows；macOS 上是钥匙串失败时的后备）。
pub struct FileStore;

impl CredentialStore for FileStore {
    fn read(&self, slot: &Slot) -> Result<Option<Secret>, AppError> {
        read_file(&slot.credentials_file())
    }

    fn write(&self, slot: &Slot, secret: &Secret) -> Result<(), AppError> {
        let path = slot.credentials_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| AppError::io(parent, source))?;
        }
        crate::config::atomic_write_private(&path, &secret.to_bytes())
    }

    fn delete(&self, slot: &Slot) -> Result<(), AppError> {
        let path = slot.credentials_file();
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(AppError::io(&path, source)),
        }
    }
}

fn read_file(path: &Path) -> Result<Option<Secret>, AppError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(None),
        Ok(bytes) => Secret::parse(&bytes).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AppError::io(path, source)),
    }
}

#[cfg(target_os = "macos")]
mod keychain;

#[cfg(test)]
pub mod memory {
    //! 测试用的内存槽位。
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct MemoryStore {
        pub slots: Mutex<HashMap<String, Secret>>,
    }

    fn key(slot: &Slot) -> String {
        format!("{slot:?}")
    }

    impl MemoryStore {
        pub fn get(&self, slot: &Slot) -> Option<Secret> {
            self.slots.lock().unwrap().get(&key(slot)).cloned()
        }
        pub fn put(&self, slot: &Slot, secret: Secret) {
            self.slots.lock().unwrap().insert(key(slot), secret);
        }
    }

    impl CredentialStore for MemoryStore {
        fn read(&self, slot: &Slot) -> Result<Option<Secret>, AppError> {
            Ok(self.get(slot))
        }
        fn write(&self, slot: &Slot, secret: &Secret) -> Result<(), AppError> {
            self.put(slot, secret.clone());
            Ok(())
        }
        fn delete(&self, slot: &Slot) -> Result<(), AppError> {
            self.slots.lock().unwrap().remove(&key(slot));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn account_slot(dir: &Path) -> Slot {
        Slot::Account(dir.to_path_buf())
    }

    #[test]
    fn secure_dir_points_at_the_claude_directory_or_the_account_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        assert_eq!(
            Slot::Default { home: home.clone() }.secure_dir(),
            home.join(".claude")
        );
        assert_eq!(account_slot(temp.path()).secure_dir(), *temp.path());
    }

    #[test]
    fn global_config_prefers_a_non_empty_legacy_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let slot = Slot::Default { home: home.clone() };
        let legacy = home.join(".claude").join(".config.json");
        fs::create_dir_all(legacy.parent().expect("legacy has a parent")).expect("create .claude");
        fs::write(&legacy, r#"{"oauthAccount":{}}"#).expect("write legacy");

        assert_eq!(slot.global_config(), legacy);
    }

    #[test]
    fn global_config_ignores_an_empty_legacy_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let slot = Slot::Default { home: home.clone() };
        fs::create_dir_all(home.join(".claude")).expect("create .claude");
        fs::write(home.join(".claude").join(".config.json"), "").expect("write empty legacy");

        assert_eq!(slot.global_config(), home.join(".claude.json"));
    }

    #[test]
    fn global_config_marks_the_account_directory_for_account_slots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let slot = account_slot(temp.path());

        // 没有旧版文件：账号目录里的 .claude.json。
        assert_eq!(slot.global_config(), temp.path().join(".claude.json"));

        // 账号目录里也可能有非空旧版 .config.json，优先用它。
        fs::write(temp.path().join(".config.json"), "{}").expect("write legacy");
        assert_eq!(slot.global_config(), temp.path().join(".config.json"));
    }

    #[test]
    fn credentials_file_sits_in_the_secure_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        assert_eq!(
            Slot::Default { home: home.clone() }.credentials_file(),
            home.join(".claude").join(".credentials.json")
        );
        assert_eq!(
            account_slot(temp.path()).credentials_file(),
            temp.path().join(".credentials.json")
        );
    }

    #[test]
    fn keychain_service_distinguishes_the_default_slot_from_accounts() {
        assert_eq!(
            Slot::Default {
                home: PathBuf::from("/home/someone")
            }
            .keychain_service(),
            "Claude Code-credentials"
        );

        let temp = tempfile::tempdir().expect("tempdir");
        let service = account_slot(temp.path()).keychain_service();
        let suffix = service
            .strip_prefix("Claude Code-credentials-")
            .expect("account slots carry a suffix");
        assert_eq!(suffix.len(), 8);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));

        let other = account_slot(&temp.path().join("elsewhere")).keychain_service();
        assert_ne!(
            service, other,
            "different directories get different services"
        );
    }

    #[test]
    fn secret_parse_accepts_only_a_json_object() {
        let Value::Object(map) = json!({"claudeAiOauth": {"refreshToken": "t"}}) else {
            unreachable!()
        };
        let secret = Secret::parse(br#"{"claudeAiOauth":{"refreshToken":"t"}}"#)
            .expect("a json object parses");
        assert_eq!(secret, Secret(map));

        for bytes in [
            &b"[]"[..],
            &b"42"[..],
            &b"\"text\""[..],
            &b"null"[..],
            &b"{"[..],
            &b""[..],
        ] {
            let error = Secret::parse(bytes).unwrap_err().to_string();
            assert!(error.contains("登录信息"), "{error}");
        }
    }

    #[test]
    fn secret_to_bytes_and_fingerprint_agree_only_for_equal_content() {
        let secret = Secret::parse(br#"{"a":1,"b":"x"}"#).expect("a json object parses");
        assert_eq!(secret.fingerprint(), secret.fingerprint());

        let Value::Object(map) = json!({"a": 1, "b": "y"}) else {
            unreachable!()
        };
        assert_ne!(secret.fingerprint(), Secret(map).fingerprint());

        // 字节内容往返之后指纹不变。
        let reparsed = Secret::parse(&secret.to_bytes()).expect("round trip parses");
        assert_eq!(reparsed.fingerprint(), secret.fingerprint());
    }

    #[test]
    fn read_file_treats_missing_and_blank_files_as_no_login() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_file(&temp.path().join("absent.json")).unwrap(), None);

        let blank = temp.path().join("blank.json");
        fs::write(&blank, b"\n\t \r\n").expect("write blank bytes");
        assert_eq!(read_file(&blank).unwrap(), None);
    }

    #[test]
    fn read_file_reports_unreadable_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broken = temp.path().join("broken.json");
        fs::write(&broken, b"not json").expect("write broken bytes");
        let error = read_file(&broken).unwrap_err().to_string();
        assert!(error.contains("登录信息"), "{error}");

        let valid = temp.path().join("valid.json");
        fs::write(&valid, br#"{"organizationUuid":"org-1"}"#).expect("write valid bytes");
        let secret = read_file(&valid).unwrap().expect("a login is stored");
        assert_eq!(secret.0["organizationUuid"], "org-1");
    }

    #[test]
    fn file_store_round_trips_and_tolerates_a_missing_delete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let slot = account_slot(temp.path());
        let store = FileStore;
        let secret = Secret::parse(br#"{"claudeAiOauth":{"refreshToken":"t"}}"#)
            .expect("a json object parses");

        assert_eq!(store.read(&slot).unwrap(), None);
        store.write(&slot, &secret).expect("write lands");
        assert_eq!(store.read(&slot).unwrap(), Some(secret));

        store.delete(&slot).expect("delete removes the file");
        assert_eq!(store.read(&slot).unwrap(), None);
        store
            .delete(&slot)
            .expect("deleting a missing file is fine");
    }

    #[test]
    fn file_store_write_creates_the_account_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let slot = account_slot(
            &temp
                .path()
                .join("claude-accounts")
                .join("abc")
                .join("config"),
        );
        let secret = Secret::parse(b"{}").expect("an empty object parses");

        FileStore
            .write(&slot, &secret)
            .expect("write through missing directories");
        assert!(slot.credentials_file().is_file());
    }
}
