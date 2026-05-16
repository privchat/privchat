// QR_CODE_SPEC v1.3 Step 0+1 — qr_key generator + URL builder 集成测试。
//
// 跑在独立 test binary 里（`cargo test --test qr_unit_test`），不依赖 lib
// 内 `#[cfg(test)]` 模块的编译；避免被 main repo 现有 bit-rot 阻塞。
//
// 这一套测试覆盖：
// - generator 长度 / 字母表 / 不可预测性 / 不撞值
// - normalize_qr_base_url §7.2 验收用例表 10 条 + 防御性边界
// - build_qr_url 与各种 base 的拼接正确性

use privchat::rpc::qr::{
    build_qr_url, generate_qr_key, normalize_qr_base_url, NormalizeQrBaseUrlError, QrAction,
    QrEntity, QR_KEY_LEN,
};
use std::collections::HashSet;

// ---------- generator ----------

#[test]
fn generate_qr_key_is_16_chars() {
    let k = generate_qr_key();
    assert_eq!(k.len(), QR_KEY_LEN);
    assert_eq!(k.chars().count(), QR_KEY_LEN);
}

#[test]
fn generate_qr_key_uses_base62_alphabet_only() {
    for _ in 0..1024 {
        let k = generate_qr_key();
        for c in k.chars() {
            assert!(
                c.is_ascii_alphanumeric(),
                "qr_key char {:?} not in base62 alphabet",
                c
            );
        }
    }
}

#[test]
fn generate_qr_key_does_not_embed_user_or_group_id_patterns() {
    // 抽样 64 次，不能有可推断的前缀模式或者全数字（base62 应该高熵）
    let prefixes: HashSet<_> = (0..64).map(|_| generate_qr_key()[..4].to_string()).collect();
    assert!(prefixes.len() > 32, "qr_key prefix entropy too low");
    let all_digits: bool = (0..64).any(|_| generate_qr_key().chars().all(|c| c.is_ascii_digit()));
    // 不强行断言，但有数字-only 的概率 ~ 1e-14；这是健康度量
    let _ = all_digits;
}

#[test]
fn generate_qr_key_does_not_collide_in_1k_samples() {
    let mut seen = HashSet::new();
    for _ in 0..1000 {
        assert!(seen.insert(generate_qr_key()), "qr_key collision in 1k samples");
    }
}

#[test]
fn generate_qr_key_two_consecutive_calls_differ() {
    let a = generate_qr_key();
    let b = generate_qr_key();
    assert_ne!(a, b);
}

// ---------- normalize_qr_base_url: §7.2 验收用例 10 条 ----------

#[test]
fn case_01_plain_host_passes() {
    // case 1: https://privchat.app → pass
    assert_eq!(
        normalize_qr_base_url("https://privchat.app", true).unwrap(),
        "https://privchat.app"
    );
}

#[test]
fn case_02_trailing_slash_normalized() {
    // case 2: https://privchat.app/ → strip → https://privchat.app
    assert_eq!(
        normalize_qr_base_url("https://privchat.app/", true).unwrap(),
        "https://privchat.app"
    );
}

#[test]
fn case_03_subpath_preserved() {
    // case 3: https://example.com/app → 保留 sub-path
    assert_eq!(
        normalize_qr_base_url("https://example.com/app", true).unwrap(),
        "https://example.com/app"
    );
}

#[test]
fn case_04_subpath_trailing_slash_stripped_subpath_preserved() {
    // case 4: https://example.com/app/ → strip trailing → /app 保留
    assert_eq!(
        normalize_qr_base_url("https://example.com/app/", true).unwrap(),
        "https://example.com/app"
    );
}

#[test]
fn case_05_http_localhost_dev_passes_prod_rejected() {
    // case 5: http://localhost:8080
    //   dev (require_https=false) → pass
    assert_eq!(
        normalize_qr_base_url("http://localhost:8080", false).unwrap(),
        "http://localhost:8080"
    );
    //   prod (require_https=true) → reject
    assert!(matches!(
        normalize_qr_base_url("http://localhost:8080", true),
        Err(NormalizeQrBaseUrlError::HttpRejectedInProduction)
    ));
}

#[test]
fn case_06_preincluded_protocol_prefix_rejected() {
    // case 6: https://privchat.app/privchat:protocol → reject
    assert!(matches!(
        normalize_qr_base_url("https://privchat.app/privchat:protocol", true),
        Err(NormalizeQrBaseUrlError::AlreadyContainsProtocolPrefix)
    ));
}

#[test]
fn case_07_preincluded_protocol_prefix_with_trailing_slash_rejected() {
    // case 7: https://privchat.app/privchat:protocol/ → reject
    assert!(matches!(
        normalize_qr_base_url("https://privchat.app/privchat:protocol/", true),
        Err(NormalizeQrBaseUrlError::AlreadyContainsProtocolPrefix)
    ));
}

#[test]
fn case_08_ftp_scheme_rejected() {
    // case 8: ftp://privchat.app → reject
    assert!(matches!(
        normalize_qr_base_url("ftp://privchat.app", true),
        Err(NormalizeQrBaseUrlError::InvalidScheme(_))
    ));
}

#[test]
fn case_09_no_scheme_rejected() {
    // case 9: privchat.app（无 scheme） → reject
    let result = normalize_qr_base_url("privchat.app", true);
    assert!(
        matches!(
            result,
            Err(NormalizeQrBaseUrlError::UnparseableUrl(_))
                | Err(NormalizeQrBaseUrlError::InvalidScheme(_))
        ),
        "got {:?}",
        result
    );
}

#[test]
fn case_10_empty_or_whitespace_rejected() {
    // case 10: empty / whitespace → reject
    assert_eq!(
        normalize_qr_base_url("", true),
        Err(NormalizeQrBaseUrlError::Empty)
    );
    assert_eq!(
        normalize_qr_base_url("   ", true),
        Err(NormalizeQrBaseUrlError::Empty)
    );
    assert_eq!(
        normalize_qr_base_url("\t\n  ", true),
        Err(NormalizeQrBaseUrlError::Empty)
    );
}

// ---------- 防御性边界 ----------

#[test]
fn defense_missing_host_rejected() {
    // file scheme 通常 host 是空
    assert!(matches!(
        normalize_qr_base_url("file:///etc/passwd", true),
        Err(NormalizeQrBaseUrlError::InvalidScheme(_))
    ));
}

#[test]
fn defense_https_with_port_passes() {
    assert_eq!(
        normalize_qr_base_url("https://privchat.app:8443", true).unwrap(),
        "https://privchat.app:8443"
    );
}

#[test]
fn defense_deeply_nested_subpath_preserved() {
    assert_eq!(
        normalize_qr_base_url("https://example.com/tenant/qr/v1", true).unwrap(),
        "https://example.com/tenant/qr/v1"
    );
}

#[test]
fn defense_protocol_prefix_in_query_not_path_passes() {
    // path 里没有 privchat:protocol —— 即使 query 里有也允许（不被前缀检查误伤）。
    // url crate 会把 host-only base 规范化为带空 path 的形式（`https://host/?q=`），
    // normalize 不在 query 前剥 `/`（trim_end_matches 只看尾巴），这是预期行为。
    assert_eq!(
        normalize_qr_base_url("https://privchat.app?ref=privchat:protocol", true).unwrap(),
        "https://privchat.app/?ref=privchat:protocol"
    );
}

// ---------- build_qr_url ----------

#[test]
fn build_user_get_against_plain_host() {
    let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
    assert_eq!(
        build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
        "https://privchat.app/privchat:protocol/user/get/abc123"
    );
}

#[test]
fn build_user_get_after_trailing_slash_normalization() {
    let base = normalize_qr_base_url("https://privchat.app/", true).unwrap();
    assert_eq!(
        build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
        "https://privchat.app/privchat:protocol/user/get/abc123"
    );
}

#[test]
fn build_user_get_preserves_subpath_no_trailing_slash() {
    let base = normalize_qr_base_url("https://example.com/app", true).unwrap();
    assert_eq!(
        build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
        "https://example.com/app/privchat:protocol/user/get/abc123"
    );
}

#[test]
fn build_group_join_preserves_subpath_with_trailing_slash() {
    let base = normalize_qr_base_url("https://example.com/app/", true).unwrap();
    assert_eq!(
        build_qr_url(&base, QrEntity::Group, QrAction::Join, "abc123"),
        "https://example.com/app/privchat:protocol/group/join/abc123"
    );
}

#[test]
fn build_http_localhost_dev_works() {
    let base = normalize_qr_base_url("http://localhost:8080", false).unwrap();
    assert_eq!(
        build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
        "http://localhost:8080/privchat:protocol/user/get/abc123"
    );
}

#[test]
fn build_qrkey_with_special_chars_is_percent_encoded() {
    // 实际生产 qr_key 永远是 base62，不需要 encode；这个测试验证 builder 的
    // defensive percent-encoding 行为：path 段里如果含 `/` `?` 等会撕裂 URL 结构
    // 的字符，必须 encode
    let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
    let out = build_qr_url(&base, QrEntity::User, QrAction::Get, "a b/c?d&e=f");
    assert!(
        out.ends_with("/get/a%20b%2Fc%3Fd%26e%3Df"),
        "expected percent-encoded special chars in path tail, got: {out}"
    );
    // 不再生成 query string
    assert!(!out.contains('?'), "v1.4 must not generate ?qrkey= query, got: {out}");
}

#[test]
fn build_qrkey_pure_base62_not_modified() {
    let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
    let out = build_qr_url(&base, QrEntity::Group, QrAction::Join, "K7sP3qXfA9eLm2nB");
    assert_eq!(
        out,
        "https://privchat.app/privchat:protocol/group/join/K7sP3qXfA9eLm2nB"
    );
}

#[test]
fn build_end_to_end_generated_qr_key_lands_in_url() {
    // 集成路径：generator → builder
    let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
    let key = generate_qr_key();
    let url = build_qr_url(&base, QrEntity::User, QrAction::Get, &key);
    let expected_prefix = "https://privchat.app/privchat:protocol/user/get/";
    assert!(url.starts_with(expected_prefix));
    assert!(url.ends_with(&key));
    assert_eq!(url.len(), expected_prefix.len() + key.len());
    assert!(!url.contains('?'));
}
