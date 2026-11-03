//! Custom endpoints management
//!
//! Handles CRUD operations for provider custom endpoints.

use crate::app_config::AppType;
use crate::error::AppError;
use crate::settings::CustomEndpoint;
use crate::store::AppState;

/// Get custom endpoints list for a provider
pub fn get_custom_endpoints(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
) -> Result<Vec<CustomEndpoint>, AppError> {
    state
        .db
        .get_custom_endpoints_by_provider(app_type.as_str(), provider_id)
}

/// Add a custom endpoint to a provider
pub fn add_custom_endpoint(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
    url: String,
) -> Result<(), AppError> {
    let normalized = url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        return Err(AppError::localized(
            "provider.endpoint.url_required",
            "URL 不能为空",
            "URL cannot be empty",
        ));
    }

    state
        .db
        .add_custom_endpoint(app_type.as_str(), provider_id, &normalized)?;
    Ok(())
}

/// Remove a custom endpoint from a provider
pub fn remove_custom_endpoint(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
    url: String,
) -> Result<(), AppError> {
    let normalized = url.trim().trim_end_matches('/').to_string();
    state
        .db
        .remove_custom_endpoint(app_type.as_str(), provider_id, &normalized)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_home_lock;
    use crate::database::Database;
    use crate::provider::Provider;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::sync::Arc;

    /// HOME 换成临时现场的守卫：`AppState` 内部要取 app config dir，不能落进真实
    /// 用户目录。
    struct EndpointsTestHome {
        _lock: tokio::sync::MutexGuard<'static, ()>,
        _temp: tempfile::TempDir,
        previous_home: Option<OsString>,
    }

    impl EndpointsTestHome {
        fn new() -> Self {
            // 和 deeplink / openclaw_config 共用一把 HOME 锁。
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

    impl Drop for EndpointsTestHome {
        fn drop(&mut self) {
            match &self.previous_home {
                Some(value) => std::env::set_var("CC_SWITCH_TEST_HOME", value),
                None => std::env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn provider_fixture() -> Provider {
        Provider {
            id: "gemini-provider".to_string(),
            name: "Fast Gemini".to_string(),
            settings_config: serde_json::json!({}),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    #[test]
    #[serial]
    fn add_rejects_a_url_with_nothing_left_after_trimming() {
        let home = EndpointsTestHome::new();
        let state = home.state();

        for url in ["", "   ", "/", "///"] {
            let error =
                add_custom_endpoint(&state, AppType::Claude, "gemini-provider", url.to_string())
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("URL 不能为空"), "{url:?} -> {error}");
        }
        assert!(
            state
                .db
                .get_custom_endpoints_by_provider("claude", "gemini-provider")
                .expect("read the endpoints")
                .is_empty(),
            "空 URL 不该落库"
        );
    }

    #[test]
    #[serial]
    fn add_normalizes_the_url_before_storing() {
        let home = EndpointsTestHome::new();
        let state = home.state();

        // provider_endpoints 对 providers 有外键：先有供应商行才能挂端点。
        state
            .db
            .save_provider("claude", &provider_fixture())
            .expect("seed the provider");

        add_custom_endpoint(
            &state,
            AppType::Claude,
            "gemini-provider",
            "  https://x.com/api///  ".to_string(),
        )
        .expect("add the endpoint");

        let saved = state
            .db
            .get_custom_endpoints_by_provider("claude", "gemini-provider")
            .expect("read the endpoints");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].url, "https://x.com/api");

        // 归一化后相同的 URL 再添一次只占一行。
        add_custom_endpoint(
            &state,
            AppType::Claude,
            "gemini-provider",
            "https://x.com/api".to_string(),
        )
        .expect("add the endpoint again");
        assert_eq!(
            state
                .db
                .get_custom_endpoints_by_provider("claude", "gemini-provider")
                .expect("read the endpoints")
                .len(),
            1
        );
    }

    #[test]
    #[serial]
    fn remove_matches_the_same_normalized_url() {
        let home = EndpointsTestHome::new();
        let state = home.state();

        // provider_endpoints 对 providers 有外键：先有供应商行才能挂端点。
        state
            .db
            .save_provider("claude", &provider_fixture())
            .expect("seed the provider");

        add_custom_endpoint(
            &state,
            AppType::Claude,
            "gemini-provider",
            "https://x.com/api".to_string(),
        )
        .expect("add the endpoint");
        remove_custom_endpoint(
            &state,
            AppType::Claude,
            "gemini-provider",
            "  https://x.com/api/  ".to_string(),
        )
        .expect("remove the endpoint");

        assert!(state
            .db
            .get_custom_endpoints_by_provider("claude", "gemini-provider")
            .expect("read the endpoints")
            .is_empty());
    }
}
