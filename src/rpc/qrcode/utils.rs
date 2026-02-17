use crate::rpc::error::RpcError;
use url::Url;

/// 从 QR 码 URL 中提取 qr_key
///
/// # 示例
/// ```
/// let qr_code = "privchat://group/get?qrkey=abc123&token=xyz";
/// let qr_key = extract_qr_key_from_url(qr_code)?;
/// assert_eq!(qr_key, "abc123");
/// ```
pub fn extract_qr_key_from_url(qr_code: &str) -> Result<String, RpcError> {
    let url = Url::parse(qr_code)
        .map_err(|e| RpcError::validation(format!("无效的 QR 码格式: {}", e)))?;

    url.query_pairs()
        .find(|(k, _): &(std::borrow::Cow<str>, std::borrow::Cow<str>)| k == "qrkey")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| RpcError::validation("QR 码缺少 qrkey 参数".to_string()))
}

/// 从 QR 码 URL 中提取 token（可选）
///
/// # 示例
/// ```
/// let qr_code = "privchat://group/get?qrkey=abc123&token=xyz";
/// let token = extract_token_from_url(qr_code);
/// assert_eq!(token, Some("xyz".to_string()));
/// ```
pub fn extract_token_from_url(qr_code: &str) -> Option<String> {
    let url = Url::parse(qr_code).ok()?;
    url.query_pairs()
        .find(|(k, _): &(std::borrow::Cow<str>, std::borrow::Cow<str>)| k == "token")
        .map(|(_, v)| v.to_string())
}

/// 生成随机 token（用于群组邀请）
pub fn generate_random_token() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();

    (0..16)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_qr_key() {
        let qr_code = "privchat://group/get?qrkey=abc123&token=xyz";
        let qr_key = extract_qr_key_from_url(qr_code).unwrap();
        assert_eq!(qr_key, "abc123");
    }

    #[test]
    fn test_extract_token() {
        let qr_code = "privchat://group/get?qrkey=abc123&token=xyz";
        let token = extract_token_from_url(qr_code).unwrap();
        assert_eq!(token, "xyz");
    }

    #[test]
    fn test_extract_qr_key_without_token() {
        let qr_code = "privchat://user/get?qrkey=def456";
        let qr_key = extract_qr_key_from_url(qr_code).unwrap();
        assert_eq!(qr_key, "def456");

        let token = extract_token_from_url(qr_code);
        assert_eq!(token, None);
    }

    #[test]
    fn test_generate_random_token() {
        let token1 = generate_random_token();
        let token2 = generate_random_token();

        assert_eq!(token1.len(), 16);
        assert_eq!(token2.len(), 16);
        assert_ne!(token1, token2);
    }
}
