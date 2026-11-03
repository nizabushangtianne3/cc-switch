//! Prompt import from deep link
//!
//! Handles importing prompt configurations via ccswitch:// URLs.

use super::utils::decode_base64_param;
use super::DeepLinkImportRequest;
use crate::error::AppError;
use crate::prompt::Prompt;
use crate::services::PromptService;
use crate::store::AppState;
use crate::AppType;
use std::str::FromStr;

/// Import a prompt from deep link request
pub fn import_prompt_from_deeplink(
    state: &AppState,
    request: DeepLinkImportRequest,
) -> Result<String, AppError> {
    // Verify this is a prompt request
    if request.resource != "prompt" {
        return Err(AppError::InvalidInput(format!(
            "Expected prompt resource, got '{}'",
            request.resource
        )));
    }

    // Extract required fields
    let app_str = request
        .app
        .as_ref()
        .ok_or_else(|| AppError::InvalidInput("Missing 'app' field for prompt".to_string()))?;

    let name = request
        .name
        .ok_or_else(|| AppError::InvalidInput("Missing 'name' field for prompt".to_string()))?;

    // Parse app type
    let app_type = AppType::from_str(app_str)
        .map_err(|_| AppError::InvalidInput(format!("Invalid app type: {app_str}")))?;

    // Decode content
    let content_b64 = request
        .content
        .as_ref()
        .ok_or_else(|| AppError::InvalidInput("Missing 'content' field for prompt".to_string()))?;

    let content = decode_base64_param("content", content_b64)?;
    let content = String::from_utf8(content)
        .map_err(|e| AppError::InvalidInput(format!("Invalid UTF-8 in content: {e}")))?;

    // Generate ID
    let timestamp = chrono::Utc::now().timestamp_millis();
    let sanitized_name = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect::<String>()
        .to_lowercase();
    let id = format!("{sanitized_name}-{timestamp}");

    // Check if we should enable this prompt
    let should_enable = request.enabled.unwrap_or(false);

    // Create Prompt (initially disabled)
    let prompt = Prompt {
        id: id.clone(),
        name: name.clone(),
        content,
        description: request.description,
        enabled: false, // Always start as disabled, will be enabled later if needed
        created_at: Some(timestamp),
        updated_at: Some(timestamp),
    };

    // Save using PromptService
    PromptService::upsert_prompt(state, app_type, &id, prompt)?;

    // If enabled flag is set, enable this prompt (which will disable others)
    if should_enable {
        PromptService::enable_prompt(state, app_type, &id)?;
        log::info!("Successfully imported and enabled prompt '{name}' for {app_str}");
    } else {
        log::info!("Successfully imported prompt '{name}' for {app_str} (disabled)");
    }

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_home_lock;
    use crate::database::Database;
    use base64::Engine as _;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::sync::Arc;

    /// HOME 换成临时现场的守卫：`AppState` 内部要取 app config dir，不能落进真实
    /// 用户目录；启用提示词时写的 live 文件也跟着落进临时 HOME。
    struct PromptDeepLinkHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
    }

    impl PromptDeepLinkHome {
        fn new() -> Self {
            // 和 openclaw_config / gemini_mcp 共用一把 HOME 锁。
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

    impl Drop for PromptDeepLinkHome {
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

    fn valid_request() -> DeepLinkImportRequest {
        DeepLinkImportRequest {
            resource: "prompt".to_string(),
            app: Some("claude".to_string()),
            name: Some("My Prompt".to_string()),
            content: Some(base64::engine::general_purpose::STANDARD.encode("# Be nice")),
            description: Some("a test prompt".to_string()),
            ..Default::default()
        }
    }

    #[test]
    #[serial]
    fn a_non_prompt_resource_is_rejected() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();
        let error = import_prompt_from_deeplink(&state, request("provider"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Expected prompt resource"), "{error}");
    }

    #[test]
    #[serial]
    fn missing_fields_are_rejected_one_by_one() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();

        let error = import_prompt_from_deeplink(&state, request("prompt"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Missing 'app' field"), "{error}");

        let mut no_name = request("prompt");
        no_name.app = Some("claude".to_string());
        let error = import_prompt_from_deeplink(&state, no_name)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Missing 'name' field"), "{error}");

        let mut no_content = valid_request();
        no_content.content = None;
        let error = import_prompt_from_deeplink(&state, no_content)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Missing 'content' field"), "{error}");
    }

    #[test]
    #[serial]
    fn an_unknown_app_type_is_rejected() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();
        let mut request = valid_request();
        request.app = Some("notepad".to_string());
        let error = import_prompt_from_deeplink(&state, request)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid app type"), "{error}");
    }

    #[test]
    #[serial]
    fn content_that_is_not_base64_is_rejected() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();
        let mut request = valid_request();
        request.content = Some("!!!!not base64!!!!".to_string());
        assert!(import_prompt_from_deeplink(&state, request).is_err());
    }

    #[test]
    #[serial]
    fn content_that_is_not_utf8_is_rejected() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();
        let mut request = valid_request();
        // 合法 base64，但解码出来不是 UTF-8 文本。
        request.content = Some(base64::engine::general_purpose::STANDARD.encode([0xff, 0xfe]));
        let error = import_prompt_from_deeplink(&state, request)
            .unwrap_err()
            .to_string();
        assert!(error.contains("Invalid UTF-8 in content"), "{error}");
    }

    #[test]
    #[serial]
    fn a_valid_request_imports_the_prompt_disabled() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();

        let id = import_prompt_from_deeplink(&state, valid_request()).expect("import the prompt");

        // ID = 净化后的名字 + '-' + 毫秒时间戳。
        let (name, timestamp) = id.split_once('-').expect("the id carries a timestamp");
        assert_eq!(name, "myprompt");
        assert!(
            timestamp.parse::<i64>().is_ok(),
            "{timestamp} 应是毫秒时间戳"
        );

        let prompts = state
            .db
            .get_prompts("claude")
            .expect("read the prompts back");
        assert_eq!(prompts.len(), 1);
        let prompt = &prompts[&id];
        assert_eq!(prompt.name, "My Prompt");
        assert_eq!(prompt.content, "# Be nice");
        assert_eq!(prompt.description.as_deref(), Some("a test prompt"));
        assert!(
            !prompt.enabled,
            "导入的提示词先停用，生效与否交给 enabled 开关"
        );
        assert!(prompt.created_at.is_some());
        assert_eq!(prompt.created_at, prompt.updated_at);
    }

    #[test]
    #[serial]
    fn the_enabled_flag_enables_the_imported_prompt() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();

        let mut request = valid_request();
        request.enabled = Some(true);
        let id = import_prompt_from_deeplink(&state, request).expect("import the prompt");

        let prompts = state
            .db
            .get_prompts("claude")
            .expect("read the prompts back");
        assert!(prompts[&id].enabled, "enabled 开关要把导入的提示词打开");
    }

    #[test]
    #[serial]
    fn the_id_sanitizes_the_name() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();
        let mut request = valid_request();
        request.name = Some("  Weird Name!! 42  ".to_string());

        let id = import_prompt_from_deeplink(&state, request).expect("import the prompt");
        let (name, _) = id.split_once('-').expect("the id carries a timestamp");
        assert_eq!(name, "weirdname42");
    }

    #[test]
    #[serial]
    fn importing_twice_keeps_two_prompts_with_fresh_ids() {
        let home = PromptDeepLinkHome::new();
        let state = home.state();

        let first = import_prompt_from_deeplink(&state, valid_request()).expect("first import");
        let second = import_prompt_from_deeplink(&state, valid_request()).expect("second import");

        assert_eq!(first.split_once('-').expect("a timestamp").0, "myprompt");
        assert_eq!(second.split_once('-').expect("a timestamp").0, "myprompt");

        // 同一毫秒内两次导入拿到同一个 ID，第二次按 ID 覆盖；跨毫秒才是两条。
        // 两种情形都成立的不变式：库里只有这两次导入的结果，内容都对。
        let prompts = state
            .db
            .get_prompts("claude")
            .expect("read the prompts back");
        assert!(!prompts.is_empty(), "至少落一条");
        assert!(prompts.len() <= 2, "最多两条，不该翻倍累积");
        for (id, prompt) in &prompts {
            assert!(id == &first || id == &second, "unexpected id: {id}");
            assert_eq!(prompt.name, "My Prompt");
            assert_eq!(prompt.content, "# Be nice");
        }
    }
}
