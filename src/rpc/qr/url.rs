// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! QR_CODE_SPEC v1.3 — 统一 URL 构造与配置规范化。
//!
//! 所有 user/group qrcode 的响应 `qr_code` 字段都必须经此模块拼接，
//! **禁止** handler 自行 `format!` URL（否则部署到 sub-path 域名时会丢路径）。
//!
//! ## 模板（v1.4：qr_key 在 path 最后一段，不再用 `?qrkey=` query）
//! ```text
//! {qr_base_url}/privchat:protocol/{entity}/{action}/{percent-encode(qr_key)}
//! ```
//!
//! ## 启动期校验
//! 配置加载时调用 [`normalize_qr_base_url`]：
//! - trim 首尾空白
//! - 去尾斜杠
//! - 必须 `http` 或 `https`（生产环境强制 `https`，由调用方根据 env 判断）
//! - **不能**已经带 `/privchat:protocol` 路径前缀（防止 builder 双拼）
//! - **允许** base 带任意其他 path 前缀（部署在 sub-path 下的场景），builder 保留
//!
//! 见 [QR_CODE_SPEC v1.4 §7.2](../../../../../privchat-docs/spec/02-server/QR_CODE_SPEC.md)
//! 验收用例表（本文件 #[test] 完整覆盖 10 条）。

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use std::fmt;

/// Path 前缀，所有 PrivChat 二维码 URL 的固定路径段。
pub const QR_PROTOCOL_PATH_PREFIX: &str = "/privchat:protocol";

/// 受支持的 entity 枚举（v1：user / group）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrEntity {
    User,
    Group,
}

impl QrEntity {
    pub fn as_str(self) -> &'static str {
        match self {
            QrEntity::User => "user",
            QrEntity::Group => "group",
        }
    }
}

impl fmt::Display for QrEntity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 受支持的 action 枚举（v1：get / join）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrAction {
    Get,
    Join,
}

impl QrAction {
    pub fn as_str(self) -> &'static str {
        match self {
            QrAction::Get => "get",
            QrAction::Join => "join",
        }
    }
}

impl fmt::Display for QrAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `qr_base_url` 启动期校验失败的具体类型。
///
/// 调用方根据变体决定错误消息或退出码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeQrBaseUrlError {
    /// 空字符串 / 仅空白。
    Empty,
    /// scheme 非 `http` / `https`，或缺少 scheme。
    InvalidScheme(String),
    /// host 缺失（例如 `https://`、`http:///` 等病态输入）。
    MissingHost,
    /// base 已经带了 `/privchat:protocol` 前缀，builder 会双拼。
    AlreadyContainsProtocolPrefix,
    /// 生产环境必须强制 https，但配置是 http。
    HttpRejectedInProduction,
    /// URL parser 完全 reject。
    UnparseableUrl(String),
}

impl fmt::Display for NormalizeQrBaseUrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "qr_base_url is empty"),
            Self::InvalidScheme(s) => {
                write!(f, "qr_base_url scheme must be http or https, got: {:?}", s)
            }
            Self::MissingHost => write!(f, "qr_base_url has no host"),
            Self::AlreadyContainsProtocolPrefix => write!(
                f,
                "qr_base_url MUST NOT already contain {}; the URL builder appends it",
                QR_PROTOCOL_PATH_PREFIX
            ),
            Self::HttpRejectedInProduction => {
                write!(f, "qr_base_url scheme http is forbidden in production")
            }
            Self::UnparseableUrl(s) => write!(f, "qr_base_url is not a valid URL: {}", s),
        }
    }
}

impl std::error::Error for NormalizeQrBaseUrlError {}

/// 启动期规范化 `qr_base_url`，返回 `Ok(normalized)` 或 [`NormalizeQrBaseUrlError`]。
///
/// 规则（见 §7.2 验收用例）：
/// - trim 首尾空白
/// - scheme 必须 `http`/`https`；`require_https=true` 时拒绝 `http`
/// - 必须有 host
/// - **去尾斜杠**：`https://x.com/` → `https://x.com`；`https://x.com/app/` → `https://x.com/app`
/// - **保留 sub-path**：`https://example.com/app` → 保留 `/app`
/// - **拒绝已包含 `/privchat:protocol`**：包括尾斜杠形态（`/privchat:protocol/`）
///
/// 输出的 `normalized` 满足：
/// - 无尾斜杠（确保 builder `{normalized}/privchat:protocol/...` 不会双斜杠）
/// - scheme 合法
/// - host 存在
/// - 不含协议前缀
pub fn normalize_qr_base_url(
    raw: &str,
    require_https: bool,
) -> Result<String, NormalizeQrBaseUrlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(NormalizeQrBaseUrlError::Empty);
    }

    let parsed = ::url::Url::parse(trimmed)
        .map_err(|e| NormalizeQrBaseUrlError::UnparseableUrl(e.to_string()))?;

    let scheme = parsed.scheme();
    match scheme {
        "http" => {
            if require_https {
                return Err(NormalizeQrBaseUrlError::HttpRejectedInProduction);
            }
        }
        "https" => {}
        other => return Err(NormalizeQrBaseUrlError::InvalidScheme(other.to_string())),
    }

    if parsed.host_str().is_none_or(|h| h.is_empty()) {
        return Err(NormalizeQrBaseUrlError::MissingHost);
    }

    // 用 url crate 已经规范化好的 base，再剥尾斜杠
    // parsed.as_str() 形如 "https://privchat.app/" 或 "https://example.com/app"
    let normalized = parsed.as_str().trim_end_matches('/').to_string();
    // 再保险：若用户输入了 "https://x.com" parsed.as_str() 会变成 "https://x.com/"，
    // trim_end_matches('/') 后变成 "https://x.com" — OK。

    // 拒绝已包含 protocol 前缀（path 等于或以 /privchat:protocol 起头）
    // 用 path() 判断更稳，避免误判 host 里包含的字符串
    let path = parsed.path(); // 已经 percent-decoded 过的标准化形式
    if path == QR_PROTOCOL_PATH_PREFIX || path.starts_with(&format!("{}/", QR_PROTOCOL_PATH_PREFIX))
    {
        return Err(NormalizeQrBaseUrlError::AlreadyContainsProtocolPrefix);
    }

    // 边界情况：trim_end_matches('/') 把 "https://x.com" 这种 host-only base 也吞掉了尾斜杠 — 这是想要的
    // 边界情况：path 为空（"https://x.com"）时 trim 后 normalized = "https://x.com" — 符合预期
    debug_assert!(!normalized.ends_with('/'), "normalized must not end with /");
    Ok(normalized)
}

/// percent-encode 字符集：完全保守，所有非 unreserved 都 encode。
///
/// base62 / base64url 都属于 unreserved，所以日常 qr_key 不会被 encode；
/// 但未来若字符集变了（含 `+/=`），URL 也不会被破坏。
const QR_KEY_PERCENT_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'\\')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'|')
    .add(b'+')
    .add(b'=');

/// 构造一个二维码 URL。
///
/// # 参数
/// - `base`：**已经规范化过**的 `qr_base_url`（必须由 [`normalize_qr_base_url`] 校验过）。
///   传入未规范化的字符串会产生预期外的输出（含尾斜杠 / 双前缀），调用方负责保证。
/// - `entity`：[`QrEntity::User`] / [`QrEntity::Group`]
/// - `action`：[`QrAction::Get`] / [`QrAction::Join`]
/// - `qr_key`：opaque token（generator 来源，调用方不做额外加工）
///
/// # 返回
/// 形如 `{base}/privchat:protocol/{entity}/{action}/{percent-encoded qr_key}` 的字符串。
///
/// **注意**：v1.3 历史形态 `?qrkey=<key>` 不再生成（但 client parser 仍兼容输入）。
pub fn build_qr_url(base: &str, entity: QrEntity, action: QrAction, qr_key: &str) -> String {
    let encoded = utf8_percent_encode(qr_key, QR_KEY_PERCENT_SET).to_string();
    format!(
        "{base}{prefix}/{entity}/{action}/{encoded}",
        base = base,
        prefix = QR_PROTOCOL_PATH_PREFIX,
        entity = entity.as_str(),
        action = action.as_str(),
        encoded = encoded,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // -------- normalize_qr_base_url: spec §7.2 验收用例（10 条） --------

    #[test]
    fn normalize_plain_host() {
        // case 1: https://privchat.app → pass
        assert_eq!(
            normalize_qr_base_url("https://privchat.app", true).unwrap(),
            "https://privchat.app"
        );
    }

    #[test]
    fn normalize_trailing_slash() {
        // case 2: https://privchat.app/ → 去尾斜杠 → https://privchat.app
        assert_eq!(
            normalize_qr_base_url("https://privchat.app/", true).unwrap(),
            "https://privchat.app"
        );
    }

    #[test]
    fn normalize_with_subpath() {
        // case 3: https://example.com/app → 保留 sub-path
        assert_eq!(
            normalize_qr_base_url("https://example.com/app", true).unwrap(),
            "https://example.com/app"
        );
    }

    #[test]
    fn normalize_with_subpath_trailing_slash() {
        // case 4: https://example.com/app/ → 去尾斜杠后保留 /app
        assert_eq!(
            normalize_qr_base_url("https://example.com/app/", true).unwrap(),
            "https://example.com/app"
        );
    }

    #[test]
    fn normalize_http_dev_ok_prod_rejected() {
        // case 5: http://localhost:8080
        //   dev (require_https=false) → pass
        //   prod (require_https=true) → reject
        assert_eq!(
            normalize_qr_base_url("http://localhost:8080", false).unwrap(),
            "http://localhost:8080"
        );
        assert!(matches!(
            normalize_qr_base_url("http://localhost:8080", true),
            Err(NormalizeQrBaseUrlError::HttpRejectedInProduction)
        ));
    }

    #[test]
    fn normalize_reject_preincluded_protocol_prefix() {
        // case 6: https://privchat.app/privchat:protocol → reject
        assert!(matches!(
            normalize_qr_base_url("https://privchat.app/privchat:protocol", true),
            Err(NormalizeQrBaseUrlError::AlreadyContainsProtocolPrefix)
        ));
    }

    #[test]
    fn normalize_reject_preincluded_protocol_prefix_trailing_slash() {
        // case 7: https://privchat.app/privchat:protocol/ → reject
        assert!(matches!(
            normalize_qr_base_url("https://privchat.app/privchat:protocol/", true),
            Err(NormalizeQrBaseUrlError::AlreadyContainsProtocolPrefix)
        ));
    }

    #[test]
    fn normalize_reject_non_http_scheme() {
        // case 8: ftp://privchat.app → reject
        assert!(matches!(
            normalize_qr_base_url("ftp://privchat.app", true),
            Err(NormalizeQrBaseUrlError::InvalidScheme(_))
        ));
    }

    #[test]
    fn normalize_reject_no_scheme() {
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
    fn normalize_reject_empty_and_whitespace() {
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

    // 额外防御性用例

    #[test]
    fn normalize_reject_missing_host() {
        // file scheme has no host
        assert!(matches!(
            normalize_qr_base_url("file:///etc/passwd", true),
            Err(NormalizeQrBaseUrlError::InvalidScheme(_))
        ));
    }

    #[test]
    fn normalize_passes_https_with_port() {
        assert_eq!(
            normalize_qr_base_url("https://privchat.app:8443", true).unwrap(),
            "https://privchat.app:8443"
        );
    }

    #[test]
    fn normalize_passes_deeply_nested_subpath() {
        assert_eq!(
            normalize_qr_base_url("https://example.com/tenant/qr/v1", true).unwrap(),
            "https://example.com/tenant/qr/v1"
        );
    }

    // -------- build_qr_url: §7.2 验收用例对应表 --------

    #[test]
    fn build_user_get_plain_host() {
        let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
        assert_eq!(
            build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
            "https://privchat.app/privchat:protocol/user/get/abc123"
        );
    }

    #[test]
    fn build_user_get_normalized_trailing_slash() {
        let base = normalize_qr_base_url("https://privchat.app/", true).unwrap();
        assert_eq!(
            build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
            "https://privchat.app/privchat:protocol/user/get/abc123"
        );
    }

    #[test]
    fn build_user_get_subpath_preserved() {
        let base = normalize_qr_base_url("https://example.com/app", true).unwrap();
        assert_eq!(
            build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
            "https://example.com/app/privchat:protocol/user/get/abc123"
        );
    }

    #[test]
    fn build_group_join_subpath_trailing_slash() {
        let base = normalize_qr_base_url("https://example.com/app/", true).unwrap();
        assert_eq!(
            build_qr_url(&base, QrEntity::Group, QrAction::Join, "abc123"),
            "https://example.com/app/privchat:protocol/group/join/abc123"
        );
    }

    #[test]
    fn build_dev_http_localhost() {
        let base = normalize_qr_base_url("http://localhost:8080", false).unwrap();
        assert_eq!(
            build_qr_url(&base, QrEntity::User, QrAction::Get, "abc123"),
            "http://localhost:8080/privchat:protocol/user/get/abc123"
        );
    }

    #[test]
    fn build_qrkey_percent_encoded_when_needed() {
        // qr_key 含特殊字符（不会自然发生，但 builder 必须 defensive 把 path 段
        // 分隔符 `/` `?` 等 encode，否则会把 URL 撕裂成多段）
        let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
        let out = build_qr_url(&base, QrEntity::User, QrAction::Get, "a b/c?d&e=f");
        // 期望尾段是 percent-encoded 后的 qr_key，紧贴 /get/ 后面
        assert!(out.ends_with("/get/a%20b%2Fc%3Fd%26e%3Df"), "got: {out}");
        // 不应混进任何 query string
        assert!(
            !out.contains('?'),
            "v1.4 must not generate query string: {out}"
        );
    }

    #[test]
    fn build_qrkey_base62_not_encoded() {
        // 正常 16-char base62 不会被 encode
        let base = normalize_qr_base_url("https://privchat.app", true).unwrap();
        let out = build_qr_url(&base, QrEntity::Group, QrAction::Join, "K7sP3qXfA9eLm2nB");
        assert_eq!(
            out,
            "https://privchat.app/privchat:protocol/group/join/K7sP3qXfA9eLm2nB"
        );
    }
}
