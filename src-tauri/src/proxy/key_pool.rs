use crate::provider::{ApiKeyEntry, ApiKeyStrategy};
use sha2::{Digest, Sha256};
use smallvec::SmallVec;

const HEX_DIGITS: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f',
];

/// Stable, non-reversible identifier for an API key.
///
/// Usage rows store this fingerprint instead of the raw key so statistics can
/// survive key edits and remain safe to inspect or sync.
pub fn api_key_fingerprint(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut fingerprint = String::with_capacity(digest.len() * 2);
    for byte in digest {
        fingerprint.push(HEX_DIGITS[usize::from(byte >> 4)]);
        fingerprint.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
    }
    fingerprint
}

/// 按调度策略给出「同一供应商内 Key 的尝试顺序」。
///
/// 返回的是 `entries` 的下标。首元素是本次请求应当使用的 Key（单 Key 选择 =
/// 取首元素），其余元素是同一个 Key 池里接下来的候选 Key：当前 Key 遇到
/// 可重试错误时按这个顺序依次顶上，池内 Key 全部失败之后才回退到供应商级
/// 故障转移。返回 `None` 表示池内没有有效 Key，调用方保留原有 fallback。
///
/// 排序规则（首元素与引入 Key 级重试之前完全一致，不改配额分布）：
/// - 轮询：从 `cursor` 指向的位置开始循环，其后的顺序稳定；
/// - 随机：从 `random_seed` 决定的位置开始循环，其后的顺序稳定；
/// - 权重：仍按权重随机决定本次用哪个 Key，其余候选按权重从大到小
///   （权重相同保持池内原顺序，稳定排序）。
///
/// 池内没有可用 Key（空池或全是空白）时返回 `None`；重复 Key 会被去掉，
/// 避免把同一个 Key 连续发两遍。
pub fn ordered_api_key_indices(
    entries: &[ApiKeyEntry],
    strategy: Option<ApiKeyStrategy>,
    cursor: usize,
    random_seed: u64,
) -> Option<SmallVec<[usize; 4]>> {
    let valid: SmallVec<[usize; 4]> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.key.trim().is_empty())
        .map(|(index, _)| index)
        .collect();

    if valid.is_empty() {
        return None;
    }

    let strategy = strategy.unwrap_or_default();
    let first_index = match strategy {
        ApiKeyStrategy::Random => (random_seed % valid.len() as u64) as usize,
        ApiKeyStrategy::Weighted => weighted_index(entries, &valid, random_seed),
        // RoundRobin（以及将来的默认策略）从游标位置开始。
        ApiKeyStrategy::RoundRobin => cursor % valid.len(),
    };
    // 首元素之后的候选顺序：权重策略按权重降序，其余按池内循环。
    let retry_pool: SmallVec<[usize; 4]> = match strategy {
        ApiKeyStrategy::Weighted => {
            let mut weighted = valid.clone();
            // 稳定排序：权重相同时保持池内原顺序，行为可预期。
            weighted.sort_by_key(|&index| std::cmp::Reverse(entries[index].weight.max(1)));
            weighted
        }
        _ => (0..valid.len())
            .map(|offset| valid[(first_index + offset) % valid.len()])
            .collect(),
    };

    // 首元素是本次请求应当使用的 Key，其余是重试候选；重复 Key 只保留一次。
    let selected_key = &entries[first_index].key;
    let mut keys: SmallVec<[usize; 4]> = SmallVec::with_capacity(valid.len());
    keys.push(first_index);
    for index in retry_pool {
        let key = &entries[index].key;
        if key != selected_key && keys.iter().all(|&candidate| &entries[candidate].key != key) {
            keys.push(index);
        }
    }
    Some(keys)
}

/// 权重策略下本次请求命中的下标：按权重做一次确定性抽签（权重即配额）。
fn weighted_index(entries: &[ApiKeyEntry], valid: &[usize], random_seed: u64) -> usize {
    let total_weight: u64 = valid
        .iter()
        .map(|&index| u64::from(entries[index].weight.max(1)))
        .sum();
    let mut point = random_seed % total_weight;
    for (position, &index) in valid.iter().enumerate() {
        let weight = u64::from(entries[index].weight.max(1));
        if point < weight {
            return position;
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
        ordered_api_key_indices(entries, strategy, cursor, random_seed)
            .and_then(|indices| indices.first().map(|&index| entries[index].key.clone()))
            .unwrap_or_else(|| fallback.to_string())
    }

    fn ordered_keys(
        entries: &[ApiKeyEntry],
        strategy: Option<ApiKeyStrategy>,
        cursor: usize,
        random_seed: u64,
    ) -> Vec<&str> {
        ordered_api_key_indices(entries, strategy, cursor, random_seed)
            .expect("test pool has a usable key")
            .into_iter()
            .map(|index| entries[index].key.as_str())
            .collect()
    }

    #[test]
    fn falls_back_when_pool_is_empty_or_blank() {
        assert!(ordered_api_key_indices(&[], None, 0, 7).is_none());
        assert!(ordered_api_key_indices(&[entry("", 1)], None, 0, 7).is_none());
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
            ordered_keys(&entries, Some(ApiKeyStrategy::RoundRobin), 1, 0),
            vec!["b", "c", "a"]
        );
    }

    #[test]
    fn key_retry_order_is_a_full_wipe_of_the_pool() {
        let entries = vec![entry("a", 1), entry("b", 1)];
        assert_eq!(
            ordered_keys(&entries, Some(ApiKeyStrategy::RoundRobin), 0, 0),
            vec!["a", "b"]
        );
        assert_eq!(
            ordered_keys(&entries, Some(ApiKeyStrategy::RoundRobin), 1, 0),
            vec!["b", "a"]
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
            ordered_keys(&entries, Some(ApiKeyStrategy::Random), 0, 4),
            vec!["b", "c", "a"]
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
            ordered_keys(&entries, Some(ApiKeyStrategy::Weighted), 0, 0),
            vec!["a", "b", "c"]
        );
        // seed=1 → 抽中 b，剩下的按权重降序接着试。
        assert_eq!(
            ordered_keys(&entries, Some(ApiKeyStrategy::Weighted), 0, 1),
            vec!["b", "c", "a"]
        );
    }

    #[test]
    fn duplicate_keys_are_not_tried_twice() {
        let entries = vec![entry("a", 1), entry("a", 1), entry("  ", 1), entry("b", 1)];
        assert_eq!(
            ordered_keys(&entries, Some(ApiKeyStrategy::RoundRobin), 0, 0),
            vec!["a", "b"]
        );
    }
}
