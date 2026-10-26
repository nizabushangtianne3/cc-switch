//! 全仓唯一的哈希与十六进制编码实现。
//!
//! 出现之前的形状：`live`、`services::s3`、`services::sync_protocol`、
//! `proxy::json_canonical`、`proxy::usage::logger`、`codex_history_migration`
//! 各自写了一份 SHA-256 → 十六进制，其中三份逐字节 `format!("{byte:02x}")`，
//! 一个摘要就要 8~32 次 `String` 分配。这里收成查表加一次分配。
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// 小写十六进制数字表。查表避免每个字节走一次格式化器。
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// 把任意长的字节串编码成小写十六进制，只分配一次。
///
/// 预分配容量后逐个 `push` ASCII 字符，不会中途再分配。
#[inline]
pub fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// 完整 SHA-256 摘要的小写十六进制（64 字符）。
#[inline]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

/// 摘要前 8 字节的小写十六进制（16 字符），用于日志和调试标识这类短指纹。
#[inline]
pub fn short_sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes)[..8])
}

/// HMAC-SHA256，定长 32 字节返回。
///
/// 返回数组而非 `Vec`：S3 / 火山方签名的密钥派生链每一步都不再堆分配。
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(mac.finalize().into_bytes().as_slice());
    out
}

/// HMAC-SHA256 的小写十六进制（64 字符）。
#[inline]
pub fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    hex_lower(&hmac_sha256(key, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_lower_matches_a_known_vector() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xf0, 0xff]), "000ff0ff");
    }

    #[test]
    fn hex_lower_empty() {
        assert_eq!(hex_lower(&[]), "");
    }

    #[test]
    fn sha256_hex_of_empty_input() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_hex_of_a_short_string() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn short_sha256_hex_takes_the_first_eight_bytes() {
        assert_eq!(short_sha256_hex(b"hello"), "2cf24dba5fb0a30e");
    }

    #[test]
    fn hmac_sha256_matches_the_rfc2104_test_vector() {
        let result = hmac_sha256(b"key", b"The quick brown fox jumps over the lazy dog");
        assert_eq!(
            hex_lower(&result),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn hmac_sha256_hex_covers_a_long_key() {
        assert_eq!(
            hmac_sha256_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
}
