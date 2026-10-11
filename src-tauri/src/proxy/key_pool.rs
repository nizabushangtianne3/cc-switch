use crate::provider::{ApiKeyEntry, ApiKeyStrategy};
use sha2::{Digest, Sha256};

/// Stable, non-reversible identifier for an API key.
///
/// Usage rows store this fingerprint instead of the raw key so statistics can
/// survive key edits and remain safe to inspect or sync.
pub fn api_key_fingerprint(key: &str) -> String {
    Sha256::digest(key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 按调度策略给出「同一供应商内 Key 的尝试顺序」。
///
/// 返回值首元素是本次请求应当使用的 Key（单 Key 选择 = 取首元素），
/// 其余元素是同一个 Key 池里接下来的候选 Key：当前 Key 遇到可重试错误时按这个
/// 顺序依次顶上，池内 Key 全部失败之后才回退到供应商级故障转移。
///
/// 排序规则（首元素与引入 Key 级重试之前完全一致，不改配额分布）：
/// - 轮询：从 `cursor` 指向的位置开始循环，其后的顺序稳定；
/// - 随机：从 `random_seed` 决定的位置开始循环，其后的顺序稳定；
/// - 权重：仍按权重随机决定本次用哪个 Key，其余候选按权重从大到小
///   （权重相同保持池内原顺序，稳定排序）。
///
/// 池内没有可用 Key（空池或全是空白）时返回 `vec![fallback]`，调用方行为与单 Key
/// 供应商一致；重复 Key 会被去掉，避免把同一个 Key 连续发两遍。
pub fn ordered_api_keys(
    entries: &[ApiKeyEntry],
    strategy: Option<ApiKeyStrategy>,
    fallback: &str,
    cursor: usize,
    random_seed: u64,
) -> Vec<String> {
    let valid: Vec<&ApiKeyEntry> = entries
        .iter()
        .filter(|entry| !entry.key.trim().is_empty())
        .collect();

    if valid.is_empty() {
        return vec![fallback.to_string()];
    }

    let strategy = strategy.unwrap_or_default();
    let first_index = match strategy {
        ApiKeyStrategy::Random => (random_seed % valid.len() as u64) as usize,
        ApiKeyStrategy::Weighted => weighted_index(&valid, random_seed),
        // RoundRobin（以及将来的默认策略）从游标位置开始。
        ApiKeyStrategy::RoundRobin => cursor % valid.len(),
    };
    // 首元素之后的候选顺序：权重策略按权重降序，其余按池内循环。
    let retry_pool: Vec<&ApiKeyEntry> = match strategy {
        ApiKeyStrategy::Weighted => {
            let mut weighted = valid.clone();
            // 稳定排序：权重相同时保持池内原顺序，行为可预期。
            weighted.sort_by_key(|entry| std::cmp::Reverse(entry.weight.max(1)));
            weighted
        }
        _ => (0..valid.len())
            .map(|offset| valid[(first_index + offset) % valid.len()])
            .collect(),
    };

    // 首元素是本次请求应当使用的 Key，其余是重试候选；重复 Key 只保留一次。
    let mut keys: Vec<String> = Vec::with_capacity(valid.len());
    keys.push(valid[first_index].key.clone());
    for entry in retry_pool {
        if entry.key != valid[first_index].key && keys.iter().all(|key| key != &entry.key) {
            keys.push(entry.key.clone());
        }
    }
    keys
}

/// 权重策略下本次请求命中的 Key：按权重做一次确定性抽签（权重即配额）。
fn weighted_index(valid: &[&ApiKeyEntry], random_seed: u64) -> usize {
    let total_weight: u64 = valid
        .iter()
        .map(|entry| u64::from(entry.weight.max(1)))
        .sum();
    let mut point = random_seed % total_weight;
    for (index, entry) in valid.iter().enumerate() {
        let weight = u64::from(entry.weight.max(1));
        if point < weight {
            return index;
        }
        point -= weight;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_fingerprint_is_stable_sha256() {
        assert_eq!(
            api_key_fingerprint("sk-test-key"),
            "0d62f396c1317066f55a96086517047c737087c61eb2bf016b72e6298927b15b"
        );
        assert_ne!(
            api_key_fingerprint("sk-test-key"),
            api_key_fingerprint("sk-other-key")
        );
    }

    #[test]
    fn missing_weight_defaults_to_one() {
        let entry: ApiKeyEntry = serde_json::from_value(serde_json::json!({
            "key": "sk-a"
        }))
        .unwrap();
        assert_eq!(entry.weight, 1);
    }

    fn entry(key: &str, weight: u32) -> ApiKeyEntry {
        ApiKeyEntry {
            key: key.to_string(),
            weight,
        }
    }

    /// 「本次请求用哪个 Key」＝尝试顺序的首元素（`ProviderRouter` 就是这么用的）。
    fn selected_key(
        entries: &[ApiKeyEntry],
        strategy: Option<ApiKeyStrategy>,
        fallback: &str,
        cursor: usize,
        random_seed: u64,
    ) -> String {
        ordered_api_keys(entries, strategy, fallback, cursor, random_seed)
            .into_iter()
            .next()
            .unwrap_or_else(|| fallback.to_string())
    }

    #[test]
    fn falls_back_when_pool_is_empty_or_blank() {
        assert_eq!(
            ordered_api_keys(&[], None, "fallback", 0, 7),
            vec!["fallback".to_string()]
        );
        assert_eq!(
            ordered_api_keys(&[entry("", 1)], None, "fallback", 0, 7),
            vec!["fallback".to_string()]
        );
        assert_eq!(
            selected_key(&[], None, "fallback", 0, 7),
            "fallback".to_string()
        );
        assert_eq!(
            selected_key(&[entry("", 1)], None, "fallback", 0, 7),
            "fallback".to_string()
        );
    }

    #[test]
    fn round_robin_rotates_from_the_cursor() {
        let entries = vec![entry("a", 1), entry("b", 1), entry("c", 1)];
        // 游标每请求推进一格：首元素依次是 a、b、c、a、b。
        let selected: Vec<String> = (0..5)
            .map(|cursor| {
                selected_key(
                    &entries,
                    Some(ApiKeyStrategy::RoundRobin),
                    "fallback",
                    cursor,
                    0,
                )
            })
            .collect();
        assert_eq!(selected, ["a", "b", "c", "a", "b"]);

        // 同一游标下，重试顺序是从该位置开始的完整循环。
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::RoundRobin), "fallback", 1, 0),
            vec!["b".to_string(), "c".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn key_retry_order_is_a_full_wipe_of_the_pool() {
        let entries = vec![entry("a", 1), entry("b", 1)];
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::RoundRobin), "fallback", 0, 0),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::RoundRobin), "fallback", 1, 0),
            vec!["b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn random_uses_the_supplied_seed() {
        let entries = vec![entry("a", 1), entry("b", 1), entry("c", 1)];
        assert_eq!(
            selected_key(&entries, Some(ApiKeyStrategy::Random), "fallback", 0, 1),
            "b"
        );
        assert_eq!(
            selected_key(&entries, Some(ApiKeyStrategy::Random), "fallback", 0, 4),
            "b"
        );
        // 随机只决定起点，之后的顺序仍是池内循环，保证池内每个 Key 都会被试到。
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::Random), "fallback", 0, 4),
            vec!["b".to_string(), "c".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn weighted_selection_respects_weights() {
        let entries = vec![entry("a", 1), entry("b", 3)];
        let selected: Vec<String> = (0..4)
            .map(|seed| {
                selected_key(
                    &entries,
                    Some(ApiKeyStrategy::Weighted),
                    "fallback",
                    0,
                    seed,
                )
            })
            .collect();
        assert_eq!(selected, ["a", "b", "b", "b"]);
    }

    #[test]
    fn weighted_retry_order_prefers_the_heaviest_key() {
        let entries = vec![entry("a", 1), entry("b", 5), entry("c", 5)];
        // 本次用哪个 Key 仍按权重抽签（seed=0 → 抽中权重最小的 a），
        // 之后的候选按权重从大到小：权重相同保持池内原顺序。
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::Weighted), "fallback", 0, 0),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        // seed=1 → 抽中 b，剩下的按权重降序接着试。
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::Weighted), "fallback", 0, 1),
            vec!["b".to_string(), "c".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn duplicate_keys_are_not_tried_twice() {
        let entries = vec![entry("a", 1), entry("a", 1), entry("  ", 1), entry("b", 1)];
        assert_eq!(
            ordered_api_keys(&entries, Some(ApiKeyStrategy::RoundRobin), "fallback", 0, 0),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}
