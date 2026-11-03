//! Skill import from deep link
//!
//! Handles importing skill repository configurations via ccswitch:// URLs.

use super::DeepLinkImportRequest;
use crate::error::AppError;
use crate::services::skill::SkillRepo;
use crate::store::AppState;

/// Import a skill from deep link request
pub fn import_skill_from_deeplink(
    state: &AppState,
    request: DeepLinkImportRequest,
) -> Result<String, AppError> {
    // Verify this is a skill request
    if request.resource != "skill" {
        return Err(AppError::InvalidInput(format!(
            "Expected skill resource, got '{}'",
            request.resource
        )));
    }

    // Parse repo
    let repo_str = request
        .repo
        .ok_or_else(|| AppError::InvalidInput("Missing 'repo' field for skill".to_string()))?;

    let parts: Vec<&str> = repo_str.split('/').collect();
    if parts.len() != 2 {
        return Err(AppError::InvalidInput(format!(
            "Invalid repo format: expected 'owner/name', got '{repo_str}'"
        )));
    }
    let owner = parts[0].to_string();
    let name = parts[1].to_string();
    let branch = request.branch.unwrap_or_else(|| "main".to_string());

    // deeplink 是不可信输入，且 branch 会被拼进归档下载 URL：URL 解析会消解点段，
    // `../../../releases/download/v1/evil` 之类会把落点改写成攻击者可上传的
    // release asset。主防线在 SkillService::download_repo，这里是入库前的纵深拦截，
    // 让用户当场看到错误而不是让脏数据沉淀进 skill_repos 表。
    crate::services::skill::SkillService::validate_repo_ref(&owner, &name, &branch).map_err(
        |_| {
            AppError::InvalidInput(format!(
                "Invalid skill repository reference: '{owner}/{name}' branch '{branch}'"
            ))
        },
    )?;

    // Create SkillRepo
    let repo = SkillRepo {
        owner: owner.clone(),
        name: name.clone(),
        branch,
        enabled: request.enabled.unwrap_or(true),
    };

    // Save using Database
    state.db.save_skill_repo(&repo)?;

    log::info!("Successfully added skill repo '{owner}/{name}'");

    Ok(format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_home_lock;
    use crate::database::Database;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::sync::Arc;

    /// HOME 换成临时现场的守卫：`AppState` 内部要取 app config dir，不能落进真实
    /// 用户目录。
    struct SkillDeepLinkHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
    }

    impl SkillDeepLinkHome {
        fn new() -> Self {
            // 和 deeplink::prompt / openclaw_config 共用一把 HOME 锁。
            let lock = test_home_lock().blocking_lock();
            let temp = tempfile::tempdir().expect("tempdir");
            let previous_home = std::env::var_os("CC_SWITCH_TEST_HOME");
            std::env::set_var("CC_SWITCH_TEST_HOME", temp.path());
            Self {
                _lock: lock,
                _temp: temp,
                previous_home,
            }
        }

        fn state(&self) -> AppState {
            AppState::new(Arc::new(Database::memory().expect("memory db")))
        }
    }

    impl Drop for SkillDeepLinkHome {
        fn drop(&mut self) {
            match &self.previous_home {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn request(resource: &str) -> DeepLinkImportRequest {
        DeepLinkImportRequest {
            resource: resource.to_string(),
            ..Default::default()
        }
    }

    fn repo_request(repo: &str) -> DeepLinkImportRequest {
        DeepLinkImportRequest {
            resource: "skill".to_string(),
            repo: Some(repo.to_string()),
            ..Default::default()
        }
    }

    #[test]
    #[serial]
    fn a_non_skill_resource_is_rejected() {
        let home = SkillDeepLinkHome::new();
        let state = home.state();
        let error = import_skill_from_deeplink(&state, request("provider"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Expected skill resource"), "{error}");
    }

    #[test]
    #[serial]
    fn a_repo_without_the_owner_name_shape_is_rejected() {
        let home = SkillDeepLinkHome::new();
        let state = home.state();

        let error = import_skill_from_deeplink(&state, repo_request("solo"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid repo format"), "{error}");

        let error = import_skill_from_deeplink(&state, repo_request("a/b/c"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid repo format"), "{error}");

        let error = import_skill_from_deeplink(&state, request("skill"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Missing 'repo' field"), "{error}");
    }

    #[test]
    #[serial]
    fn a_repo_whose_reference_breaks_the_archive_url_is_rejected() {
        let home = SkillDeepLinkHome::new();
        let state = home.state();

        let mut request = repo_request("owner/repo");
        // branch 会被拼进归档下载 URL，点段和分隔符都要在入库前拦下。
        request.branch = Some("../../../releases/download/v1/evil".to_string());
        let error = import_skill_from_deeplink(&state, request)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Invalid skill repository reference"),
            "{error}"
        );

        // 空的 name 同理：形状上是两段，但坐标本身不合法。
        let error = import_skill_from_deeplink(&state, repo_request("owner/"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Invalid skill repository reference"),
            "{error}"
        );

        assert!(
            state
                .db
                .get_skill_repos()
                .expect("read the repos")
                .is_empty(),
            "校验不过就不该落库"
        );
    }

    #[test]
    #[serial]
    fn a_valid_repo_is_saved_with_the_main_branch_and_enabled_by_default() {
        let home = SkillDeepLinkHome::new();
        let state = home.state();

        let key = import_skill_from_deeplink(&state, repo_request("farion1231/cc-switch"))
            .expect("import the repo");
        assert_eq!(key, "farion1231/cc-switch");

        let repos = state.db.get_skill_repos().expect("read the repos");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].owner, "farion1231");
        assert_eq!(repos[0].name, "cc-switch");
        assert_eq!(repos[0].branch, "main", "链接没带 branch 时默认 main");
        assert!(repos[0].enabled, "链接没带开关时默认启用");
    }

    #[test]
    #[serial]
    fn the_branch_and_enabled_flag_come_from_the_request() {
        let home = SkillDeepLinkHome::new();
        let state = home.state();

        let mut request = repo_request("farion1231/cc-switch");
        request.branch = Some("release/v1.2.3".to_string());
        request.enabled = Some(false);
        import_skill_from_deeplink(&state, request).expect("import the repo");

        let repos = state.db.get_skill_repos().expect("read the repos");
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].branch, "release/v1.2.3");
        assert!(!repos[0].enabled);
    }

    #[test]
    #[serial]
    fn importing_the_same_repo_twice_keeps_one_row() {
        let home = SkillDeepLinkHome::new();
        let state = home.state();

        let mut first = repo_request("farion1231/cc-switch");
        first.branch = Some("dev".to_string());
        import_skill_from_deeplink(&state, first).expect("import the repo");
        import_skill_from_deeplink(&state, repo_request("farion1231/cc-switch"))
            .expect("import the repo again");

        let repos = state.db.get_skill_repos().expect("read the repos");
        assert_eq!(repos.len(), 1, "同一仓库只占一行，后来的覆盖先前的");
        assert_eq!(repos[0].branch, "main");
    }
}
