//! 凭据（[`Secret`]）上的小工具：账号键的拆合、指纹、认人、过期时间。

use serde_json::{Map, Value};

use super::store::{CredentialStore, Secret, Slot};
use super::{identity, State, ACCOUNT_KEYS};
use crate::error::AppError;
use crate::services::claude_account::AccountPlan;

/// `base` 的非账号键 + `account` 的账号键。
pub(super) fn merge(base: Option<&Secret>, account: &Secret) -> Secret {
    let mut map = base
        .and_then(without_account_keys)
        .map(|secret| secret.0)
        .unwrap_or_default();
    for key in ACCOUNT_KEYS {
        if let Some(value) = account.0.get(*key) {
            map.insert((*key).to_string(), value.clone());
        }
    }
    Secret(map)
}

/// 去掉账号键后剩下的（MCP 授权等，属于这台电脑）；什么都不剩时为 `None`。
fn without_account_keys(secret: &Secret) -> Option<Secret> {
    let rest: Map<String, Value> = secret
        .0
        .iter()
        .filter(|(key, _)| !ACCOUNT_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    (!rest.is_empty()).then_some(Secret(rest))
}

/// 把槽位里 `secret` 的账号键去掉：剩下的写回，什么都不剩就删掉。
pub(super) fn strip_account_keys(
    store: &dyn CredentialStore,
    slot: &Slot,
    secret: &Secret,
) -> Result<(), AppError> {
    match without_account_keys(secret) {
        Some(rest) => store.write(slot, &rest),
        None => store.delete(slot),
    }
}

/// 写入后读回比对，不一致就返回 `failed()` 的错误。
pub(super) fn write_verified(
    store: &dyn CredentialStore,
    slot: &Slot,
    secret: &Secret,
    failed: impl FnOnce() -> AppError,
) -> Result<(), AppError> {
    store.write(slot, secret)?;
    if store.read(slot)?.as_ref().map(Secret::fingerprint) != Some(secret.fingerprint()) {
        return Err(failed());
    }
    Ok(())
}

/// 只算账号键的指纹（共享键可能被 Claude Code 正常改写，不影响「是谁」）。
pub(super) fn account_fingerprint(secret: &Secret) -> String {
    let subset: Map<String, Value> = ACCOUNT_KEYS
        .iter()
        .filter_map(|key| Some(((*key).to_string(), secret.0.get(*key)?.clone())))
        .collect();
    Secret(subset).fingerprint()
}

pub(super) fn credential_org(secret: &Secret) -> Option<String> {
    secret
        .0
        .get("organizationUuid")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn has_login(secret: &Secret) -> bool {
    secret
        .0
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("refreshToken"))
        .and_then(Value::as_str)
        .is_some_and(|token| !token.is_empty())
}

/// 登录（refresh token）什么时候过期，毫秒。没有这个字段时为 `None`。
pub(super) fn login_expiry(secret: &Secret) -> Option<i64> {
    let value = secret
        .0
        .get("claudeAiOauth")?
        .get("refreshTokenExpiresAt")?;
    let at = value
        .as_i64()
        .or_else(|| value.as_f64().map(|at| at as i64))?;
    // 秒级的时间戳换成毫秒
    Some(if at < 100_000_000_000 { at * 1000 } else { at })
}

/// 凭据里 Claude Code 记的套餐（非机密字段）。
pub(super) fn credential_plan(secret: &Secret) -> AccountPlan {
    let oauth = secret.0.get("claudeAiOauth");
    let field = |name: &str| {
        oauth
            .and_then(|oauth| oauth.get(name))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    AccountPlan {
        subscription_type: field("subscriptionType"),
        rate_limit_tier: field("rateLimitTier"),
    }
}

/// 把 `account` 的登录过期时间按 `secret` 记进状态（没有这个字段就删掉旧记录）。
pub(super) fn note_expiry(state: &mut State, account: &str, secret: &Secret) {
    match login_expiry(secret) {
        Some(at) => {
            state.login_expiry.insert(account.to_string(), at);
        }
        None => {
            state.login_expiry.remove(account);
        }
    }
}

/// 槽位里能用的登录：有 refreshToken、没过期。
pub(super) fn read_login(
    store: &dyn CredentialStore,
    slot: &Slot,
) -> Result<Option<Secret>, AppError> {
    let now = chrono::Utc::now().timestamp_millis();
    Ok(store
        .read(slot)?
        .filter(|secret| has_login(secret) && login_expiry(secret).is_none_or(|at| at > now)))
}

/// 一份登录是谁：凭据加同一目录的 `.claude.json`。没有登录时为 `None`；有登录但认不出（没有
/// `oauthAccount`、凭据和资料的组织对不上）时报错，免得把它收错地方。
pub(super) fn owner_of(
    secret: Option<&Secret>,
    profile: Option<&Value>,
) -> Result<Option<identity::Identity>, AppError> {
    let Some(secret) = secret.filter(|secret| has_login(secret)) else {
        return Ok(None);
    };
    let found = profile.and_then(identity::identity_from_profile);
    let profile_org = profile
        .and_then(|doc| doc.get("oauthAccount"))
        .and_then(|account| account.get("organizationUuid"))
        .and_then(Value::as_str);
    let credential_org = secret.0.get("organizationUuid").and_then(Value::as_str);
    let orgs_agree = match (credential_org, profile_org) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    match found {
        Some(found) if orgs_agree => Ok(Some(found)),
        _ => Err(AppError::localized(
            "claude.snapshot.owner_unknown",
            "认不出 Claude Code 现在登录的是哪个账号（登录资料不完整或不一致），没有切换。运行一次 claude 让它补全资料后再试",
            "Can't tell which account Claude Code is signed in to right now (its profile is missing or inconsistent), so nothing was switched. Run claude once so it can fill in the profile, then try again.",
        )),
    }
}

/// 读完之后 Claude Code 又改了登录（刷新、登录、MCP 授权）：放弃这次写入。
pub(super) fn changed() -> AppError {
    AppError::localized(
        "claude.snapshot.changed",
        "Claude Code 刚刚改动了登录信息，切换已取消，请再试一次",
        "Claude Code just changed its sign-in data, so the switch was canceled. Try again.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    fn secret(org: &str, refresh: &str) -> Secret {
        let Value::Object(map) = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": format!("at-{refresh}"),
                "refreshToken": refresh,
                "subscriptionType": "max",
                "rateLimitTier": "default_claude_max_20x"
            },
            "organizationUuid": org,
            "mcpOAuth": {"server": "kept-per-slot"}
        }) else {
            unreachable!()
        };
        Secret(map)
    }

    fn profile(account: &str, org: &str) -> Value {
        serde_json::json!({
            "oauthAccount": {
                "accountUuid": account,
                "organizationUuid": org,
                "emailAddress": format!("{account}@x.com")
            }
        })
    }

    fn slot() -> Slot {
        Slot::Account(PathBuf::from("/tmp/work"))
    }

    /// 写进去什么、读出来什么的存储。
    struct HonestStore(Mutex<Option<Secret>>);

    impl CredentialStore for HonestStore {
        fn read(&self, _slot: &Slot) -> Result<Option<Secret>, AppError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn write(&self, _slot: &Slot, secret: &Secret) -> Result<(), AppError> {
            *self.0.lock().unwrap() = Some(secret.clone());
            Ok(())
        }
        fn delete(&self, _slot: &Slot) -> Result<(), AppError> {
            *self.0.lock().unwrap() = None;
            Ok(())
        }
    }

    /// 写入被悄悄丢弃、读出来永远是 `stored` 的存储。
    struct DroppingStore(Secret);

    impl CredentialStore for DroppingStore {
        fn read(&self, _slot: &Slot) -> Result<Option<Secret>, AppError> {
            Ok(Some(self.0.clone()))
        }
        fn write(&self, _slot: &Slot, _secret: &Secret) -> Result<(), AppError> {
            Ok(())
        }
        fn delete(&self, _slot: &Slot) -> Result<(), AppError> {
            Ok(())
        }
    }

    #[test]
    fn merge_keeps_the_bases_shared_keys_and_takes_the_accounts_login_keys() {
        let base = secret("org-base", "base-token");
        let account = secret("org-target", "target-token");

        let merged = merge(Some(&base), &account);

        // 账号键整组换成 target 的。
        assert_eq!(merged.0["organizationUuid"], "org-target");
        assert_eq!(merged.0["claudeAiOauth"]["refreshToken"], "target-token");
        // 这台机器的共享键留在 base 那份里。
        assert_eq!(merged.0["mcpOAuth"]["server"], "kept-per-slot");
    }

    #[test]
    fn merge_without_a_base_starts_from_the_account_only() {
        let account = secret("org-target", "target-token");
        let merged = merge(None, &account);
        // 没有 base 可继承：只带账号自己的登录键，共享键不会凭空出现。
        assert_eq!(merged.0["claudeAiOauth"]["refreshToken"], "target-token");
        assert_eq!(merged.0["organizationUuid"], "org-target");
        let foreign: Vec<&String> = merged
            .0
            .keys()
            .filter(|key| !ACCOUNT_KEYS.contains(&key.as_str()))
            .collect();
        assert!(
            foreign.is_empty(),
            "no shared key should appear: {foreign:?}"
        );
    }

    #[test]
    fn account_fingerprint_counts_only_the_account_keys() {
        let account = secret("org-1", "token-1");
        let fingerprint = account_fingerprint(&account);

        // 共享键被 Claude Code 改写不改变指纹。
        let mut shared_changed = account.0.clone();
        shared_changed.insert(
            "mcpOAuth".to_string(),
            serde_json::json!({"server": "other"}),
        );
        assert_eq!(account_fingerprint(&Secret(shared_changed)), fingerprint);

        // 账号键变了指纹必须变。
        let other = secret("org-1", "token-2");
        assert_ne!(account_fingerprint(&other), fingerprint);
    }

    #[test]
    fn has_login_needs_a_non_empty_refresh_token() {
        assert!(has_login(&secret("org", "token")));

        let mut no_refresh = secret("org", "token").0;
        no_refresh.remove("claudeAiOauth");
        assert!(!has_login(&Secret(no_refresh)));

        let mut blank = secret("org", "token").0;
        blank.insert(
            "claudeAiOauth".to_string(),
            serde_json::json!({"refreshToken": ""}),
        );
        assert!(!has_login(&Secret(blank)));
    }

    #[test]
    fn login_expiry_normalizes_seconds_to_milliseconds() {
        let with_seconds = |at: Value| {
            let mut map = secret("org", "token").0;
            map.get_mut("claudeAiOauth")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("refreshTokenExpiresAt".to_string(), at);
            Secret(map)
        };

        // 秒级（< 1e11）补三个零。
        assert_eq!(
            login_expiry(&with_seconds(serde_json::json!(1_700_000_000))),
            Some(1_700_000_000_000)
        );
        // 毫秒级原样。
        assert_eq!(
            login_expiry(&with_seconds(serde_json::json!(1_700_000_000_000i64))),
            Some(1_700_000_000_000)
        );
        // 浮点也认。
        assert_eq!(
            login_expiry(&with_seconds(serde_json::json!(1_700_000_000.0))),
            Some(1_700_000_000_000)
        );
        // 没有这个字段就没有期限。
        assert_eq!(login_expiry(&secret("org", "token")), None);
    }

    #[test]
    fn credential_org_and_plan_read_the_non_secret_fields() {
        let account = secret("org-1", "token");
        assert_eq!(credential_org(&account).as_deref(), Some("org-1"));

        let plan = credential_plan(&account);
        assert_eq!(plan.subscription_type.as_deref(), Some("max"));
        assert_eq!(
            plan.rate_limit_tier.as_deref(),
            Some("default_claude_max_20x")
        );

        // 空串当没有填。
        let mut map = account.0.clone();
        map.get_mut("claudeAiOauth")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("subscriptionType".to_string(), serde_json::json!("  "));
        assert_eq!(credential_plan(&Secret(map)).subscription_type, None);
    }

    #[test]
    fn note_expiry_records_and_drops_the_timestamp() {
        let mut state = State::default();
        let account = secret("org-1", "token");
        let mut with_expiry = account.0.clone();
        with_expiry
            .get_mut("claudeAiOauth")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "refreshTokenExpiresAt".to_string(),
                serde_json::json!(1_700_000_000),
            );

        note_expiry(&mut state, "work", &Secret(with_expiry));
        assert_eq!(state.login_expiry.get("work"), Some(&1_700_000_000_000));

        // 没有过期时间的账号删掉旧记录，不留过期提示。
        note_expiry(&mut state, "work", &account);
        assert!(!state.login_expiry.contains_key("work"));
    }

    #[test]
    fn owner_of_needs_a_usable_login_and_a_matching_profile() {
        // 没有登录：不知道是谁，但也不是错误。
        assert_eq!(
            owner_of(None, Some(&profile("acct", "org-1"))).unwrap(),
            None
        );

        let account = secret("org-1", "token");
        let found = owner_of(Some(&account), Some(&profile("acct", "org-1")))
            .unwrap()
            .expect("matching profile identifies the login");
        assert!(found.key.contains("acct"), "{found:?}");

        // 组织对不上：宁可报错也不把它收错地方。
        let error = owner_of(Some(&account), Some(&profile("acct", "org-2")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("认不出"), "{error}");

        // 有登录但没有资料：同样报错。
        assert!(owner_of(Some(&account), None).is_err());
    }

    #[test]
    fn owner_of_ignores_a_login_it_cannot_see() {
        let mut no_login = secret("org-1", "token").0;
        no_login.remove("claudeAiOauth");
        assert_eq!(owner_of(Some(&Secret(no_login)), None).unwrap(), None);
    }

    #[test]
    fn write_verified_rejects_a_write_that_does_not_land() {
        let store = HonestStore(Mutex::new(None));
        let account = secret("org-1", "token");
        write_verified(&store, &slot(), &account, || {
            AppError::Message("write failed".to_string())
        })
        .unwrap();
        assert!(store.read(&slot()).unwrap().is_some());

        let silent = DroppingStore(secret("org-other", "other-token"));
        let error = write_verified(&silent, &slot(), &account, || {
            AppError::Message("write failed".to_string())
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "write failed");
    }

    #[test]
    fn strip_account_keys_writes_the_rest_or_deletes_the_slot() {
        let store = HonestStore(Mutex::new(None));
        let account = secret("org-1", "token");

        // 有共享键剩下：写回剩下的部分。
        strip_account_keys(&store, &slot(), &account).unwrap();
        let left = store
            .read(&slot())
            .unwrap()
            .expect("slot keeps the shared keys");
        assert!(left.0.contains_key("mcpOAuth"));
        assert!(!left.0.contains_key("organizationUuid"));
        assert!(!left.0.contains_key("claudeAiOauth"));

        // 什么都不剩：整个槽位删掉。
        let Value::Object(only) = serde_json::json!({"organizationUuid": "org-1"}) else {
            unreachable!()
        };
        let only_account = Secret(only);
        strip_account_keys(&store, &slot(), &only_account).unwrap();
        assert!(store.read(&slot()).unwrap().is_none());
    }

    #[test]
    fn read_login_filters_blank_and_expired_tokens() {
        let usable = secret("org-1", "token");
        let store = HonestStore(Mutex::new(Some(usable)));
        assert!(read_login(&store, &slot()).unwrap().is_some());

        let mut expired = secret("org-1", "token").0;
        expired
            .get_mut("claudeAiOauth")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(
                "refreshTokenExpiresAt".to_string(),
                serde_json::json!(1_000),
            );
        let store = HonestStore(Mutex::new(Some(Secret(expired))));
        assert!(read_login(&store, &slot()).unwrap().is_none());

        let store = HonestStore(Mutex::new(None));
        assert!(read_login(&store, &slot()).unwrap().is_none());
    }
}
