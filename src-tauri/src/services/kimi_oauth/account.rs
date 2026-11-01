//! Account identity and quota for Kimi official logins.
//!
//! Identities (`/me`: user id, nickname, avatar) are cached per provider id in
//! `kimi_accounts.json` (0600, device-local, never cloud-synced). The cache only
//! labels accounts; Kimi's config and credential files stay the source of truth.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    account_provider, account_region, credential_path, identify, lock, network_error,
    parse_credential, read_config, request, response_json, Endpoints, KimiRegion, PROVIDER_ID,
};
use crate::config::atomic_write_private;
use crate::services::subscription::{CredentialStatus, SubscriptionQuota};

const STORE_FILENAME: &str = "kimi_accounts.json";
/// Serializes read-modify-write of the identity cache across async callers.
static STORE_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn store_path() -> PathBuf {
    crate::config::get_app_config_dir().join(STORE_FILENAME)
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(super) struct Identity {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Digest of the refresh token the identity was read with. A different
    /// token may be a different login, so the account is identified again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
pub(super) struct Store {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    pub accounts: BTreeMap<String, Identity>,
    /// Native logins the user removed from the list (user id, or the provider
    /// id when the identity was unknown). CC Switch never logs Kimi out.
    #[serde(default)]
    pub dismissed: Vec<String>,
}

impl Store {
    pub fn load(path: &Path) -> Store {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        atomic_write_private(path, &bytes).map_err(|e| e.to_string())
    }

    pub fn is_dismissed(&self, id: &str) -> bool {
        id == PROVIDER_ID
            && (self.dismissed.iter().any(|key| key == PROVIDER_ID)
                || self
                    .accounts
                    .get(id)
                    .is_some_and(|identity| self.dismissed.contains(&identity.user_id)))
    }
}

/// Apply one change to the cache under the store lock.
pub(super) fn update_store(path: &Path, change: impl FnOnce(&mut Store)) -> Result<(), String> {
    let _guard = STORE_LOCK.lock().map_err(|e| e.to_string())?;
    let mut store = Store::load(path);
    store.version = 1;
    change(&mut store);
    store.save(path)
}

/// Remember who holds `id` after its token changed (Kimi rotates the refresh
/// token on every refresh). A hidden native login stays hidden while the same
/// account holds the slot; another account taking it over shows again.
pub(super) fn remember(path: &Path, id: &str, identity: Identity) -> Result<(), String> {
    update_store(path, |store| {
        if id == PROVIDER_ID && store.dismissed.iter().any(|key| key == PROVIDER_ID) {
            // Hidden before we knew who it was: pin the hide to that account.
            store.dismissed.retain(|key| key != PROVIDER_ID);
            let previous = store.accounts.get(id).map(|known| known.user_id.as_str());
            if previous.is_none_or(|user| user == identity.user_id)
                && !store.dismissed.contains(&identity.user_id)
            {
                store.dismissed.push(identity.user_id.clone());
            }
        }
        store.accounts.insert(id.into(), identity);
    })
}

/// Remember an account the user just signed in to through CC Switch, which
/// always brings a hidden native login back.
pub(super) fn remember_login(path: &Path, id: &str, identity: Identity) -> Result<(), String> {
    update_store(path, |store| {
        if id == PROVIDER_ID {
            store
                .dismissed
                .retain(|key| key != PROVIDER_ID && key != &identity.user_id);
        }
        store.accounts.insert(id.into(), identity);
    })
}

/// Stable separate-slot suffix for one account in one region, so the same
/// login maps to the same provider id on every device.
pub(super) fn account_suffix(region: KimiRegion, user_id: &str) -> String {
    let region = match region {
        KimiRegion::MainlandCn => "cn",
        KimiRegion::Global => "global",
    };
    let digest = Sha256::digest(format!("{region}:{user_id}").as_bytes());
    format!("{digest:x}")[..8].to_string()
}

pub(super) fn token_marker(credential: &Value) -> Option<String> {
    let refresh = credential["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())?;
    Some(format!("{:x}", Sha256::digest(refresh.as_bytes()))[..16].to_string())
}

fn string_field(payload: &Value, key: &str) -> Option<String> {
    match &payload[key] {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub(super) async fn fetch_identity(
    base_url: &str,
    access_token: &str,
    device_id: &str,
) -> Result<Identity, String> {
    let endpoint = format!("{base_url}/me");
    let response = identify(crate::proxy::http_client::get().get(&endpoint), device_id)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| network_error("Kimi account lookup failed", &endpoint, e))?;
    let (status, payload) = response_json(response).await?;
    if status != 200 {
        return Err(format!("Kimi account lookup failed (HTTP {status})"));
    }
    Ok(Identity {
        user_id: string_field(&payload, "user_id")
            .or_else(|| string_field(&payload, "id"))
            .ok_or("Kimi account lookup returned no user id")?,
        nickname: string_field(&payload, "nickname"),
        avatar: string_field(&payload, "avatar"),
        token: None,
    })
}

pub(super) enum Credential {
    Missing,
    Revoked,
    Valid(Value),
}

pub(super) fn read_credential(path: &Path) -> Result<Credential, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Credential::Missing),
        Err(e) => return Err(format!("Cannot read Kimi Code credentials: {e}")),
    };
    let credential: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid Kimi Code credentials; sign in again".to_string())?;
    let empty = |key: &str| credential[key].as_str().is_none_or(str::is_empty);
    // Kimi writes this tombstone after the server rejects a refresh token.
    if empty("access_token") && empty("refresh_token") {
        return Ok(Credential::Revoked);
    }
    Ok(Credential::Valid(credential))
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn access_valid(credential: &Value) -> bool {
    credential["access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
        && credential["expires_at"]
            .as_i64()
            .is_some_and(|expires| expires > now())
}

/// Kimi's own rule: refresh once less than max(5 min, half the lifetime) remains.
fn should_refresh(credential: &Value) -> bool {
    let expires_at = credential["expires_at"].as_i64().unwrap_or(0);
    if expires_at == 0 {
        return false;
    }
    let expires_in = credential["expires_in"].as_i64().unwrap_or(0);
    let threshold = if expires_in > 0 {
        300.max(expires_in / 2)
    } else {
        300
    };
    expires_at - now() < threshold
}

pub(super) enum Access {
    Token(String),
    /// The access token expired and this machine cannot refresh it safely.
    Expired,
    NeedsReauth,
}

fn tombstone(prior: &Value) -> Value {
    json!({
        "access_token": "", "refresh_token": "", "expires_at": 0, "expires_in": 0,
        "scope": prior["scope"].as_str().unwrap_or(""),
        "token_type": prior["token_type"].as_str().unwrap_or("Bearer"),
    })
}

fn write_credential(path: &Path, credential: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(credential).map_err(|e| e.to_string())?;
    atomic_write_private(path, &bytes).map_err(|e| e.to_string())
}

/// A usable access token for `provider`, refreshing it under Kimi's lock with
/// Kimi's protocol (re-read inside the lock; on rejection, adopt a token another
/// process rotated meanwhile, otherwise write the revoked tombstone).
pub(super) async fn access_token(
    home: &Path,
    provider: &Value,
    device_id: &str,
) -> Result<Access, String> {
    let path = credential_path(home, &provider["oauth"])?;
    let credential = match read_credential(&path)? {
        Credential::Missing | Credential::Revoked => return Ok(Access::NeedsReauth),
        Credential::Valid(credential) => credential,
    };
    if !should_refresh(&credential) {
        return Ok(Access::Token(
            credential["access_token"].as_str().unwrap_or("").into(),
        ));
    }
    if !lock::refresh_supported() {
        return Ok(if access_valid(&credential) {
            Access::Token(credential["access_token"].as_str().unwrap().into())
        } else {
            Access::Expired
        });
    }
    let name = slot_name(&path);
    let _lock = lock::acquire(home, &name).await?;
    let current = match read_credential(&path)? {
        Credential::Missing | Credential::Revoked => return Ok(Access::NeedsReauth),
        Credential::Valid(credential) => credential,
    };
    if !should_refresh(&current) {
        return Ok(Access::Token(
            current["access_token"].as_str().unwrap_or("").into(),
        ));
    }
    let Some(refresh_token) = current["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return Ok(Access::NeedsReauth);
    };
    let endpoints = Endpoints {
        oauth_host: provider["oauth"]["oauth_host"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| account_region(provider).endpoints().oauth_host),
        base_url: String::new(),
    };
    let response = request(&endpoints, "/api/oauth/token", device_id)
        .form(&[
            ("client_id", super::CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
        ])
        .send()
        .await
        .map_err(|e| network_error("Kimi token refresh failed", &endpoints.oauth_host, e))?;
    let (status, payload) = response_json(response).await?;
    if status == 200 {
        let refreshed = parse_credential(&payload)?;
        write_credential(&path, &refreshed)?;
        return Ok(Access::Token(
            refreshed["access_token"].as_str().unwrap().into(),
        ));
    }
    if matches!(status, 401 | 403) || payload["error"] == "invalid_grant" {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Credential::Valid(recovered) = read_credential(&path)? {
            if recovered["refresh_token"].as_str() != Some(refresh_token.as_str()) {
                return Ok(Access::Token(
                    recovered["access_token"].as_str().unwrap_or("").into(),
                ));
            }
        }
        write_credential(&path, &tombstone(&current))?;
        return Ok(Access::NeedsReauth);
    }
    Err(format!("Kimi token refresh failed (HTTP {status})"))
}

/// Credential file stem, which is also Kimi's lock name.
pub(super) fn slot_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Identify accounts whose current login has not been looked up yet. Only
/// uses access tokens that are still valid; never refreshes just for a name.
pub(super) async fn identify_accounts(home: &Path, store: &Path) {
    let Ok(config) = read_config(home) else {
        return;
    };
    // Created lazily: listing accounts must not write into Kimi's home when
    // there is nothing to identify.
    let mut device: Option<String> = None;
    let known = Store::load(store).accounts;
    for (id, provider) in config["providers"].as_object().into_iter().flatten() {
        if !crate::kimi_config::is_official_provider(&json!({"provider": provider})) {
            continue;
        }
        let Ok(path) = credential_path(home, &provider["oauth"]) else {
            continue;
        };
        let Ok(Credential::Valid(credential)) = read_credential(&path) else {
            continue;
        };
        let marker = token_marker(&credential);
        if known
            .get(id)
            .is_some_and(|identity| identity.token == marker)
            || !access_valid(&credential)
        {
            continue;
        }
        let Some(base_url) = provider["base_url"].as_str() else {
            continue;
        };
        let access = credential["access_token"].as_str().unwrap_or_default();
        if device.is_none() {
            let Ok(id) = super::device_id(home) else {
                return;
            };
            device = Some(id);
        }
        let device_id = device.as_deref().unwrap();
        if let Ok(mut identity) = fetch_identity(base_url, access, device_id).await {
            identity.token = marker;
            let _ = remember(store, id, identity);
        }
    }
}

fn quota_status(status: CredentialStatus, message: &str) -> SubscriptionQuota {
    SubscriptionQuota {
        tool: "kimi_oauth".into(),
        credential_status: status,
        credential_message: Some(message.into()),
        success: false,
        tiers: vec![],
        extra_usage: None,
        reset_credits: None,
        credits_balance: None,
        error: None,
        queried_at: Some(chrono::Utc::now().timestamp_millis()),
    }
}

pub(super) async fn quota(
    home: &Path,
    store: &Path,
    account_id: &str,
    device_id: &str,
) -> Result<SubscriptionQuota, String> {
    let config = read_config(home)?;
    // A card synced from another device has no node or no login here: show
    // nothing instead of an error.
    let not_here = || SubscriptionQuota {
        credential_message: None,
        ..quota_status(CredentialStatus::NotFound, "")
    };
    let Ok(provider) = account_provider(&config, account_id) else {
        return Ok(not_here());
    };
    if matches!(
        read_credential(&credential_path(home, &provider["oauth"])?)?,
        Credential::Missing
    ) {
        return Ok(not_here());
    }
    let base_url = provider["base_url"]
        .as_str()
        .ok_or("Kimi account has no base URL")?;
    let token = match access_token(home, provider, device_id).await? {
        Access::Token(token) => token,
        Access::Expired => {
            return Ok(quota_status(
                CredentialStatus::RefreshPending,
                "Use this account once in Kimi Code, then refresh",
            ))
        }
        Access::NeedsReauth => return Ok(quota_status(CredentialStatus::Expired, "Sign in again")),
    };
    // A refresh may have rotated the token; label the account with it too.
    if let Ok(Credential::Valid(credential)) =
        read_credential(&credential_path(home, &provider["oauth"])?)
    {
        let marker = token_marker(&credential);
        if Store::load(store)
            .accounts
            .get(account_id)
            .is_none_or(|identity| identity.token != marker)
        {
            if let Ok(mut identity) = fetch_identity(base_url, &token, device_id).await {
                identity.token = marker;
                let _ = remember(store, account_id, identity);
            }
        }
    }
    let mut quota = crate::services::coding_plan::query_kimi_at(base_url, &token).await?;
    quota.tool = "kimi_oauth".into();
    Ok(quota)
}

/// Fail early when a Kimi official card points at a login this machine does
/// not have (e.g. a profile synced from another device).
pub(crate) fn ensure_signed_in(home: &Path, oauth: &Value) -> Result<(), String> {
    match read_credential(&credential_path(home, oauth)?)? {
        Credential::Valid(_) => Ok(()),
        Credential::Missing | Credential::Revoked => Err("not signed in".into()),
    }
}

/// Delete a separate-slot credential under Kimi's lock. The native default
/// slot (`kimi-code`, `kimi-code-env-*`) belongs to Kimi and is never deleted.
pub(crate) fn remove_separate_credential_blocking(
    home: &Path,
    oauth: &Value,
) -> Result<(), String> {
    let path = credential_path(home, oauth)?;
    let name = slot_name(&path);
    if !name.starts_with("kimi-code-ccs-") {
        return Ok(());
    }
    let _lock = lock::acquire_blocking(home, &name)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot remove Kimi Code credentials: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir_all(home.path().join("credentials")).unwrap();
        home
    }

    fn credential_at(home: &Path, name: &str, credential: &Value) -> PathBuf {
        let path = home.join("credentials").join(format!("{name}.json"));
        fs::write(&path, credential.to_string()).unwrap();
        path
    }

    fn oauth(key: &str) -> Value {
        json!({"key": key})
    }

    fn live(extra_seconds: i64, expires_in: Option<i64>) -> Value {
        let mut credential = json!({"access_token": "tok", "expires_at": chrono::Utc::now().timestamp() + extra_seconds});
        if let Some(expires_in) = expires_in {
            credential["expires_in"] = json!(expires_in);
        }
        credential
    }

    #[test]
    fn read_credential_separates_missing_revoked_and_valid_files() {
        let home = temp_home();
        let path = home.path().join("credentials/kimi-code.json");
        assert!(matches!(
            read_credential(&path).unwrap(),
            Credential::Missing
        ));

        fs::write(&path, "{invalid").unwrap();
        // Credential 没 derive Debug：unwrap_err 用不了，就地把 Err 断言掉。
        match read_credential(&path) {
            Err(error) => assert!(error.contains("Invalid Kimi Code credentials"), "{error}"),
            Ok(_) => panic!("invalid JSON must be refused"),
        }

        // Kimi 登出后写的墓碑：两个令牌都空。
        let path = credential_at(
            home.path(),
            "kimi-code",
            &json!({"access_token": "", "refresh_token": ""}),
        );
        assert!(matches!(
            read_credential(&path).unwrap(),
            Credential::Revoked
        ));

        // 只空一个还算有效。
        let path = credential_at(
            home.path(),
            "kimi-code",
            &json!({"access_token": "a", "refresh_token": ""}),
        );
        let valid = read_credential(&path).unwrap();
        assert!(matches!(valid, Credential::Valid(ref v) if v["access_token"] == "a"));
    }

    #[test]
    fn access_valid_needs_a_token_and_a_future_expiry() {
        let now = chrono::Utc::now().timestamp();
        assert!(access_valid(&live(60, None)));
        assert!(!access_valid(&live(-1, None)));
        assert!(!access_valid(
            &json!({"access_token": "", "expires_at": now + 60})
        ));
        assert!(!access_valid(&json!({"access_token": "tok"})));
    }

    #[test]
    fn should_refresh_follows_kimis_own_rule() {
        // 没写过期时间：不知道什么时候过期，不刷新。
        assert!(!should_refresh(&json!({"access_token": "tok"})));
        assert!(!should_refresh(
            &json!({"access_token": "tok", "expires_at": 0})
        ));

        // 默认看 300 秒；剩得比它少就刷（整秒时钟下用 299 才稳定）。
        assert!(should_refresh(&live(299, None)));
        assert!(!should_refresh(&live(600, None)));

        // 寿命过半超过 300 秒时按半条命算：expires_in=1000 的阈值是 500。
        assert!(should_refresh(&live(30, Some(1000))));
        assert!(!should_refresh(&live(600, Some(1000))));

        // 半条命不到 300 秒时保底 300。
        assert!(!should_refresh(&live(600, Some(200))));
    }

    #[test]
    fn the_revoked_tombstone_blanks_tokens_and_keeps_the_scope() {
        let revoked = tombstone(&json!({
            "access_token": "a", "refresh_token": "r", "expires_at": 5, "expires_in": 6,
            "scope": "openid profile", "token_type": "Bot", "extra": "drop",
        }));
        assert_eq!(revoked["access_token"], "");
        assert_eq!(revoked["refresh_token"], "");
        assert_eq!(revoked["expires_at"], 0);
        assert_eq!(revoked["expires_in"], 0);
        assert_eq!(revoked["scope"], "openid profile");
        assert_eq!(revoked["token_type"], "Bot");
        assert!(revoked.get("extra").is_none());

        // 没写 token_type 时默认 Bearer。
        let default = tombstone(&json!({"scope": "s"}));
        assert_eq!(default["token_type"], "Bearer");
        assert_eq!(default["scope"], "s");
    }

    #[test]
    fn slot_name_is_the_credential_file_stem() {
        assert_eq!(
            slot_name(Path::new("/x/kimi-code-ccs-abcd.json")),
            "kimi-code-ccs-abcd"
        );
        assert_eq!(slot_name(Path::new("/x/kimi-code")), "kimi-code");
        assert_eq!(slot_name(Path::new("")), "");
    }

    #[test]
    fn the_separate_slot_suffix_is_stable_and_region_specific() {
        let first = account_suffix(KimiRegion::MainlandCn, "user-1");
        assert_eq!(first, account_suffix(KimiRegion::MainlandCn, "user-1"));
        assert_eq!(first.len(), 8);
        assert_ne!(first, account_suffix(KimiRegion::Global, "user-1"));
        assert_ne!(first, account_suffix(KimiRegion::MainlandCn, "user-2"));
    }

    #[test]
    fn the_token_marker_follows_only_the_refresh_token() {
        let marker =
            token_marker(&json!({"refresh_token": "r1", "access_token": "a"})).expect("marker");
        assert_eq!(marker.len(), 16);
        assert!(marker.chars().all(|c| c.is_ascii_hexdigit()), "{marker}");
        assert_ne!(
            marker,
            token_marker(&json!({"refresh_token": "r2"})).unwrap()
        );
        assert_eq!(token_marker(&json!({"refresh_token": ""})), None);
        assert_eq!(token_marker(&json!({"access_token": "a"})), None);
    }

    #[test]
    fn string_field_reads_trimmed_strings_and_numbers_only() {
        assert_eq!(
            string_field(&json!({"a": "  x "}), "a").as_deref(),
            Some("x")
        );
        assert_eq!(string_field(&json!({"a": 5}), "a").as_deref(), Some("5"));
        assert_eq!(string_field(&json!({"a": "   "}), "a"), None);
        assert_eq!(string_field(&json!({"a": ""}), "a"), None);
        assert_eq!(string_field(&json!({"a": true}), "a"), None);
        assert_eq!(string_field(&json!({}), "a"), None);
    }

    #[test]
    fn the_identity_cache_round_trips_through_the_store_file() {
        let home = temp_home();
        let path = home.path().join("kimi_accounts.json");
        assert!(Store::load(&path).accounts.is_empty());

        let identity = Identity {
            user_id: "u1".into(),
            nickname: Some("Nick".into()),
            ..Default::default()
        };
        remember(&path, PROVIDER_ID, identity).unwrap();

        let store = Store::load(&path);
        assert_eq!(store.version, 1);
        assert_eq!(store.accounts[PROVIDER_ID].user_id, "u1");
        assert!(!store.is_dismissed(PROVIDER_ID));
        assert!(!store.is_dismissed("kimi-code-ccs-abcd"));
    }

    #[test]
    fn hiding_the_native_login_pins_the_hide_to_its_account() {
        let home = temp_home();
        let path = home.path().join("kimi_accounts.json");
        update_store(&path, |store| store.dismissed.push(PROVIDER_ID.into())).unwrap();

        let first = Identity {
            user_id: "u1".into(),
            ..Default::default()
        };
        remember(&path, PROVIDER_ID, first).unwrap();
        let store = Store::load(&path);
        assert!(store.is_dismissed(PROVIDER_ID), "藏过又认出来了，接着藏");
        assert!(!store.dismissed.iter().any(|key| key == PROVIDER_ID));
        assert!(store.dismissed.iter().any(|key| key == "u1"));

        // 换了个号占这个槽位：重新显示。
        let second = Identity {
            user_id: "u2".into(),
            ..Default::default()
        };
        remember(&path, PROVIDER_ID, second).unwrap();
        assert!(!Store::load(&path).is_dismissed(PROVIDER_ID));
    }

    #[test]
    fn signing_in_through_cc_switch_unhides_the_native_login() {
        let home = temp_home();
        let path = home.path().join("kimi_accounts.json");
        update_store(&path, |store| {
            store.dismissed = vec![PROVIDER_ID.into(), "u1".into(), "keep".into()];
        })
        .unwrap();

        let identity = Identity {
            user_id: "u1".into(),
            ..Default::default()
        };
        remember_login(&path, PROVIDER_ID, identity).unwrap();

        let store = Store::load(&path);
        assert!(!store.is_dismissed(PROVIDER_ID));
        // 别的号的隐藏不动。
        assert!(store.dismissed.iter().any(|key| key == "keep"));

        // 非官方槽位的隐藏与登录无关。
        update_store(&path, |store| store.dismissed.push("other-user".into())).unwrap();
        let identity = Identity {
            user_id: "u9".into(),
            ..Default::default()
        };
        remember_login(&path, "kimi-code-ccs-abcd", identity).unwrap();
        assert!(Store::load(&path)
            .dismissed
            .iter()
            .any(|key| key == "other-user"));
    }

    #[test]
    fn only_separate_slot_credentials_are_removed() {
        let home = temp_home();
        let native = credential_at(
            home.path(),
            "kimi-code",
            &json!({"access_token": "a", "refresh_token": "r"}),
        );
        // 原生槽位和 Kimi 自己的环境槽位：CC Switch 不碰。
        remove_separate_credential_blocking(home.path(), &oauth("oauth/kimi-code")).unwrap();
        assert!(native.exists());

        // 自己的槽位：删掉，再删一次也不算错。
        let separate = credential_at(
            home.path(),
            "kimi-code-ccs-abcd",
            &json!({"access_token": "a", "refresh_token": "r"}),
        );
        remove_separate_credential_blocking(home.path(), &oauth("oauth/kimi-code-ccs-abcd"))
            .unwrap();
        assert!(!separate.exists());
        remove_separate_credential_blocking(home.path(), &oauth("oauth/kimi-code-ccs-abcd"))
            .unwrap();
    }

    #[test]
    fn a_card_without_its_credential_is_not_signed_in() {
        let home = temp_home();
        assert_eq!(
            ensure_signed_in(home.path(), &oauth("oauth/kimi-code")).unwrap_err(),
            "not signed in"
        );

        let tombstone = home.path().join("credentials/kimi-code.json");
        fs::write(
            &tombstone,
            json!({"access_token": "", "refresh_token": ""}).to_string(),
        )
        .unwrap();
        assert_eq!(
            ensure_signed_in(home.path(), &oauth("oauth/kimi-code")).unwrap_err(),
            "not signed in"
        );

        fs::write(
            &tombstone,
            json!({"access_token": "a", "refresh_token": "r"}).to_string(),
        )
        .unwrap();
        ensure_signed_in(home.path(), &oauth("oauth/kimi-code")).unwrap();
    }

    #[test]
    fn the_quota_row_is_always_a_failure_with_its_message() {
        let quota = quota_status(CredentialStatus::RefreshPending, "use it once");
        assert_eq!(quota.tool, "kimi_oauth");
        assert!(matches!(
            quota.credential_status,
            CredentialStatus::RefreshPending
        ));
        assert_eq!(quota.credential_message.as_deref(), Some("use it once"));
        assert!(!quota.success);
        assert!(quota.tiers.is_empty());
        assert!(quota.error.is_none());
        assert!(quota.queried_at.is_some_and(|at| at > 0));
    }
}
