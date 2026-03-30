use crate::rpc::error::{RpcError, RpcResult};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadReceiptMode {
    Disabled,
    CountOnly,
    FullList,
}

impl ReadReceiptMode {
    pub fn parse(raw: Option<&str>) -> RpcResult<Self> {
        let Some(raw) = raw else {
            return Ok(Self::FullList);
        };
        match raw {
            "disabled" => Ok(Self::Disabled),
            "count_only" => Ok(Self::CountOnly),
            "full_list" => Ok(Self::FullList),
            _ => Err(RpcError::validation(format!(
                "invalid read_receipt_mode: {} (expect disabled|count_only|full_list)",
                raw
            ))),
        }
    }
}

pub fn parse_read_receipt_mode(body: &Value) -> RpcResult<ReadReceiptMode> {
    ReadReceiptMode::parse(body.get("read_receipt_mode").and_then(|v| v.as_str()))
}

pub fn ensure_read_stats_allowed(mode: ReadReceiptMode) -> RpcResult<()> {
    match mode {
        ReadReceiptMode::Disabled => Err(RpcError::forbidden(
            "read receipt disabled for this channel".to_string(),
        )),
        ReadReceiptMode::CountOnly | ReadReceiptMode::FullList => Ok(()),
    }
}

pub fn ensure_read_list_allowed(mode: ReadReceiptMode) -> RpcResult<()> {
    match mode {
        ReadReceiptMode::Disabled => Err(RpcError::forbidden(
            "read receipt disabled for this channel".to_string(),
        )),
        ReadReceiptMode::CountOnly => Err(RpcError::forbidden(
            "read list is not allowed in count_only mode".to_string(),
        )),
        ReadReceiptMode::FullList => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mode_defaults_to_full_list() {
        assert_eq!(ReadReceiptMode::parse(None).expect("parse none"), ReadReceiptMode::FullList);
    }

    #[test]
    fn parse_mode_rejects_invalid_value() {
        assert!(ReadReceiptMode::parse(Some("foo")).is_err());
    }

    #[test]
    fn disabled_denies_both_stats_and_list() {
        assert!(ensure_read_stats_allowed(ReadReceiptMode::Disabled).is_err());
        assert!(ensure_read_list_allowed(ReadReceiptMode::Disabled).is_err());
    }

    #[test]
    fn count_only_allows_stats_but_denies_list() {
        assert!(ensure_read_stats_allowed(ReadReceiptMode::CountOnly).is_ok());
        assert!(ensure_read_list_allowed(ReadReceiptMode::CountOnly).is_err());
    }

    #[test]
    fn full_list_allows_stats_and_list() {
        assert!(ensure_read_stats_allowed(ReadReceiptMode::FullList).is_ok());
        assert!(ensure_read_list_allowed(ReadReceiptMode::FullList).is_ok());
    }
}
