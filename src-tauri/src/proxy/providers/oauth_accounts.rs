//! OAuth 账号库的公共挑选规则。
//!
//! Codex / Copilot / xAI 三个 OAuth manager 各存一份账号表，账号结构体不同，但
//! 「没有可用默认账号时退回哪个」的规则完全一样。这里按一份实现收拢，差异只由
//! 调用方传入的时间戳闭包表达。

use std::collections::HashMap;

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

#[cfg(test)]
mod tests {
    use super::*;

    struct Account {
        authenticated_at: i64,
        requires_reauth: bool,
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
}
