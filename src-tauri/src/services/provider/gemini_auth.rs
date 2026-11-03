//! Gemini authentication type detection
//!
//! Detects whether a Gemini provider uses PackyCode API Key, Google OAuth, or generic API Key.

use crate::provider::Provider;

/// Gemini authentication type enumeration
///
/// Used to optimize performance by avoiding repeated provider type detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeminiAuthType {
    /// PackyCode provider (uses API Key)
    Packycode,
    /// Google Official (uses OAuth)
    GoogleOfficial,
    /// Generic Gemini provider (uses API Key)
    Generic,
}

// Partner Promotion Key constants
const PACKYCODE_PARTNER_KEY: &str = "packycode";
const GOOGLE_OFFICIAL_PARTNER_KEY: &str = "google-official";

// PackyCode keyword constants
const PACKYCODE_KEYWORDS: [&str; 3] = ["packycode", "packyapi", "packy"];

/// Detect Gemini provider authentication type
///
/// One-time detection to avoid repeated calls to `is_packycode_gemini` and `is_google_official_gemini`.
///
/// # Returns
///
/// - `GeminiAuthType::GoogleOfficial`: Google official, uses OAuth
/// - `GeminiAuthType::Packycode`: PackyCode provider, uses API Key
/// - `GeminiAuthType::Generic`: Other generic providers, uses API Key
pub(crate) fn detect_gemini_auth_type(provider: &Provider) -> GeminiAuthType {
    // Priority 1: Check partner_promotion_key (most reliable)
    if let Some(key) = provider
        .meta
        .as_ref()
        .and_then(|meta| meta.partner_promotion_key.as_deref())
    {
        if key.eq_ignore_ascii_case(GOOGLE_OFFICIAL_PARTNER_KEY) {
            return GeminiAuthType::GoogleOfficial;
        }
        if key.eq_ignore_ascii_case(PACKYCODE_PARTNER_KEY) {
            return GeminiAuthType::Packycode;
        }
    }

    // Priority 2: Check Google Official (name matching)
    let name_lower = provider.name.to_ascii_lowercase();
    if name_lower == "google" || name_lower.starts_with("google ") {
        return GeminiAuthType::GoogleOfficial;
    }

    // Priority 3: Check PackyCode keywords
    if contains_packycode_keyword(&provider.name) {
        return GeminiAuthType::Packycode;
    }

    if let Some(site) = provider.website_url.as_deref() {
        if contains_packycode_keyword(site) {
            return GeminiAuthType::Packycode;
        }
    }

    if let Some(base_url) = provider
        .settings_config
        .pointer("/env/GOOGLE_GEMINI_BASE_URL")
        .and_then(|v| v.as_str())
    {
        if contains_packycode_keyword(base_url) {
            return GeminiAuthType::Packycode;
        }
    }

    GeminiAuthType::Generic
}

/// Check if string contains PackyCode related keywords (case-insensitive)
///
/// Keyword list: ["packycode", "packyapi", "packy"]
fn contains_packycode_keyword(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    PACKYCODE_KEYWORDS
        .iter()
        .any(|keyword| lower.contains(keyword))
}

/// Detect if provider is Google Official Gemini (uses OAuth authentication)
///
/// Google Official Gemini uses OAuth personal authentication, no API Key needed.
///
/// This is a convenience wrapper around `detect_gemini_auth_type`.
pub(crate) fn is_google_official_gemini(provider: &Provider) -> bool {
    detect_gemini_auth_type(provider) == GeminiAuthType::GoogleOfficial
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderMeta;
    use serde_json::json;

    fn provider(name: &str) -> Provider {
        Provider {
            id: "gemini-provider".to_string(),
            name: name.to_string(),
            settings_config: json!({}),
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

    fn with_partner_key(name: &str, key: &str) -> Provider {
        let mut provider = provider(name);
        provider.meta = Some(ProviderMeta {
            partner_promotion_key: Some(key.to_string()),
            ..Default::default()
        });
        provider
    }

    #[test]
    fn the_partner_key_outranks_every_guess() {
        // 名字看着像 PackyCode，合作标记说了算。
        let official = with_partner_key("PackyCode Fast", "google-official");
        assert_eq!(
            detect_gemini_auth_type(&official),
            GeminiAuthType::GoogleOfficial
        );
        assert!(is_google_official_gemini(&official));

        // 大小写不敏感。
        let packy = with_partner_key("Whatever", "PackyCode");
        assert_eq!(detect_gemini_auth_type(&packy), GeminiAuthType::Packycode);
        assert!(!is_google_official_gemini(&packy));

        // 不认识的标记不参与判定，继续按名字认。
        let other = with_partner_key("PackyCode Fast", "some-campaign");
        assert_eq!(detect_gemini_auth_type(&other), GeminiAuthType::Packycode);
    }

    #[test]
    fn the_name_marks_google_official_only_as_a_whole_word() {
        for name in ["Google", "google", "Google Pro", "google vertex"] {
            assert_eq!(
                detect_gemini_auth_type(&provider(name)),
                GeminiAuthType::GoogleOfficial,
                "must treat {name:?} as the official card"
            );
        }

        // 名字里带上 Google 但不是官方卡。
        for name in ["MyGoogleProxy", "Proxy Google", "Googleish"] {
            assert_eq!(
                detect_gemini_auth_type(&provider(name)),
                GeminiAuthType::Generic,
                "must not treat {name:?} as the official card"
            );
        }
    }

    #[test]
    fn packycode_keywords_are_found_in_the_name_site_and_base_url() {
        assert_eq!(
            detect_gemini_auth_type(&provider("PackyAPI Gateway")),
            GeminiAuthType::Packycode
        );

        let mut by_site = provider("Fast Gemini");
        by_site.website_url = Some("https://packycode.com".to_string());
        assert_eq!(detect_gemini_auth_type(&by_site), GeminiAuthType::Packycode);

        let mut by_base_url = provider("Fast Gemini");
        by_base_url.settings_config = json!({
            "env": {"GOOGLE_GEMINI_BASE_URL": "https://api.packy.cn/v1"}
        });
        assert_eq!(
            detect_gemini_auth_type(&by_base_url),
            GeminiAuthType::Packycode
        );
    }

    #[test]
    fn an_unremarkable_provider_is_generic() {
        assert_eq!(
            detect_gemini_auth_type(&provider("My Relay")),
            GeminiAuthType::Generic
        );
        assert!(!is_google_official_gemini(&provider("My Relay")));
    }
}
