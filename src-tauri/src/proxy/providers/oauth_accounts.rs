//! OAuth 账号库的公共挑选规则。
//!
//! Codex / Copilot / xAI 三个 OAuth manager 各存一份账号表，账号结构体不同，但
//! 「没有可用默认账号时退回哪个」的规则完全一样。这里按一份实现收拢，差异只由
//! 调用方传入的时间戳闭包表达。

use std::collections::HashMap;

use super::copilot_auth::GitHubAccount;
use tokio::sync::RwLock;

/// 显式默认账号缺失或已失效时，退回最近认证的那个账号。
///
/// `authenticated_at` 返回 `None` 表示该账号当前不可用（例如 xAI 的账号落在
/// `requires_reauth` 状态），直接不参与挑选。
///
/// 时间相同取 id 字典序**更小**的那个：`max_by` 遇到全等的两个元素时取迭代中
/// 后出现的那一个，而 `HashMap` 的遍历顺序不确定，所以必须把比较键补成全序，
/// 否则同一个账号集合在不同次运行里可能选出不同的默认账号。
pub(crate) fn fallback_default_account_id<T>(
    accounts: &HashMap<String, T>,
    authenticated_at: impl Fn(&T) -> Option<i64>,
) -> Option<String> {
    accounts
        .iter()
        .filter_map(|(id, account)| Some((id, authenticated_at(account)?)))
        .max_by(|(id_a, at_a), (id_b, at_b)| at_a.cmp(at_b).then_with(|| id_b.cmp(id_a)))
        .map(|(id, _)| id.clone())
}

/// 解析"当前该用哪个账号"：显式默认优先，缺省或已失效时退回
/// [`fallback_default_account_id`]。
///
/// 两个锁按先 `default_account_id` 后 `accounts` 的顺序取，与三个 manager 原来的
/// 顺序一致；换顺序会让按另一顺序持锁的路径之间产生死锁面。
pub(crate) async fn resolve_default_account_id<T>(
    default_account_id: &RwLock<Option<String>>,
    accounts: &RwLock<HashMap<String, T>>,
    authenticated_at: impl Fn(&T) -> Option<i64>,
) -> Option<String> {
    let stored = default_account_id.read().await.clone();
    let accounts = accounts.read().await;

    if let Some(id) = stored {
        if accounts.contains_key(&id) {
            return Some(id);
        }
    }

    fallback_default_account_id(&accounts, authenticated_at)
}

/// 账号列表的展示排序：默认账号置顶 → 认证时间新的在前 → login。
///
/// 只有 Codex 与 Copilot 两家共用：它们的展示类型都是 `GitHubAccount`、排序键也
/// 一致。xAI 多一级 `requires_reauth`，agy 是升序且不置顶默认账号，各自不同，
/// 不要往这里凑。
pub(crate) fn sorted_accounts<T>(
    accounts: &HashMap<String, T>,
    default_account_id: Option<&str>,
) -> Vec<GitHubAccount>
where
    for<'a> GitHubAccount: From<&'a T>,
{
    let mut list: Vec<GitHubAccount> = accounts.values().map(GitHubAccount::from).collect();
    list.sort_by(|a, b| {
        let a_default = default_account_id == Some(a.id.as_str());
        let b_default = default_account_id == Some(b.id.as_str());
        b_default
            .cmp(&a_default)
            .then_with(|| b.authenticated_at.cmp(&a.authenticated_at))
            .then_with(|| a.login.cmp(&b.login))
    });
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Account {
        authenticated_at: i64,
        requires_reauth: bool,
    }

    /// 只为让 `sorted_accounts` 的可测存储类型。真实三家各自的存储结构体都是私
    /// 有的，这里用一个测试替身 + `From` 把展示层接上。
    struct ListingEntry {
        id: String,
        login: String,
        authenticated_at: i64,
    }

    impl From<&ListingEntry> for GitHubAccount {
        fn from(entry: &ListingEntry) -> Self {
            GitHubAccount {
                id: entry.id.clone(),
                login: entry.login.clone(),
                avatar_url: None,
                authenticated_at: entry.authenticated_at,
                github_domain: "github.com".to_string(),
                reauth_required: false,
            }
        }
    }

    fn account(authenticated_at: i64) -> Account {
        Account {
            authenticated_at,
            requires_reauth: false,
        }
    }

    fn usable_stamp(a: &Account) -> Option<i64> {
        (!a.requires_reauth).then_some(a.authenticated_at)
    }

    #[test]
    fn picks_the_most_recently_authenticated_account() {
        let accounts = HashMap::from([
            ("older".to_string(), account(10)),
            ("newest".to_string(), account(30)),
            ("middle".to_string(), account(20)),
        ]);

        assert_eq!(
            fallback_default_account_id(&accounts, |a| Some(a.authenticated_at)),
            Some("newest".to_string())
        );
    }

    #[test]
    fn breaks_timestamp_ties_by_the_smaller_id_so_the_result_is_stable() {
        // 每次新建 map：`HashMap::new` 每回都换 RandomState 种子，迭代顺序因此
        // 不同。结果必须一直是同一个，才证明比较键补成了全序。
        for _ in 0..64 {
            let accounts = HashMap::from([
                ("zzz".to_string(), account(7)),
                ("aaa".to_string(), account(7)),
                ("mmm".to_string(), account(7)),
            ]);

            assert_eq!(
                fallback_default_account_id(&accounts, |a| Some(a.authenticated_at)),
                Some("aaa".to_string())
            );
        }
    }

    #[test]
    fn breaks_ties_among_the_maximum_not_among_every_account() {
        let accounts = HashMap::from([
            ("bbb".to_string(), account(7)),
            ("aaa".to_string(), account(9)),
            ("ddd".to_string(), account(9)),
            ("ccc".to_string(), account(1)),
        ]);

        assert_eq!(
            fallback_default_account_id(&accounts, |a| Some(a.authenticated_at)),
            Some("aaa".to_string())
        );
    }

    #[test]
    fn skips_accounts_the_caller_marks_unusable() {
        let mut blocked = account(100);
        blocked.requires_reauth = true;
        let accounts = HashMap::from([
            ("blocked".to_string(), blocked),
            ("live".to_string(), account(5)),
        ]);

        assert_eq!(
            fallback_default_account_id(&accounts, usable_stamp),
            Some("live".to_string())
        );
    }

    #[test]
    fn returns_none_when_every_account_is_unusable() {
        let mut blocked = account(100);
        blocked.requires_reauth = true;
        let accounts = HashMap::from([("blocked".to_string(), blocked)]);

        assert_eq!(fallback_default_account_id(&accounts, usable_stamp), None);
    }

    #[test]
    fn returns_none_for_an_empty_account_map() {
        let accounts: HashMap<String, Account> = HashMap::new();

        assert_eq!(fallback_default_account_id(&accounts, usable_stamp), None);
    }

    #[test]
    fn pins_the_default_account_first_and_orders_the_rest_by_recency_then_login() {
        let accounts = HashMap::from([
            (
                "old".to_string(),
                ListingEntry {
                    id: "old".to_string(),
                    login: "bravo".to_string(),
                    authenticated_at: 10,
                },
            ),
            (
                "default".to_string(),
                ListingEntry {
                    id: "default".to_string(),
                    login: "alpha".to_string(),
                    authenticated_at: 1,
                },
            ),
            (
                "newer".to_string(),
                ListingEntry {
                    id: "newer".to_string(),
                    login: "zulu".to_string(),
                    authenticated_at: 30,
                },
            ),
            (
                "mid".to_string(),
                ListingEntry {
                    id: "mid".to_string(),
                    login: "yankee".to_string(),
                    authenticated_at: 20,
                },
            ),
        ]);

        let list = sorted_accounts(&accounts, Some("default"));

        assert_eq!(
            list.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["default", "newer", "mid", "old"]
        );
    }

    #[test]
    fn falls_back_to_login_order_when_timestamps_tie() {
        let accounts = HashMap::from([
            (
                "b".to_string(),
                ListingEntry {
                    id: "b".to_string(),
                    login: "zulu".to_string(),
                    authenticated_at: 5,
                },
            ),
            (
                "a".to_string(),
                ListingEntry {
                    id: "a".to_string(),
                    login: "alpha".to_string(),
                    authenticated_at: 5,
                },
            ),
        ]);

        let list = sorted_accounts(&accounts, None);

        assert_eq!(
            list.iter().map(|a| a.login.as_str()).collect::<Vec<_>>(),
            ["alpha", "zulu"]
        );
    }

    #[tokio::test]
    async fn keeps_the_stored_default_when_it_still_exists() {
        let default_account_id = RwLock::new(Some("stored".to_string()));
        let accounts = RwLock::new(HashMap::from([("stored".to_string(), account(1))]));

        assert_eq!(
            resolve_default_account_id(&default_account_id, &accounts, |a| Some(
                a.authenticated_at
            ))
            .await,
            Some("stored".to_string())
        );
    }

    #[tokio::test]
    async fn falls_back_when_the_stored_default_no_longer_exists() {
        let default_account_id = RwLock::new(Some("gone".to_string()));
        let accounts = RwLock::new(HashMap::from([("live".to_string(), account(1))]));

        assert_eq!(
            resolve_default_account_id(&default_account_id, &accounts, |a| Some(
                a.authenticated_at
            ))
            .await,
            Some("live".to_string())
        );
    }

    #[tokio::test]
    async fn resolves_straight_to_the_fallback_when_nothing_is_stored() {
        let default_account_id: RwLock<Option<String>> = RwLock::new(None);
        let accounts = RwLock::new(HashMap::from([
            ("older".to_string(), account(1)),
            ("newer".to_string(), account(2)),
        ]));

        assert_eq!(
            resolve_default_account_id(&default_account_id, &accounts, |a| Some(
                a.authenticated_at
            ))
            .await,
            Some("newer".to_string())
        );
    }
}
