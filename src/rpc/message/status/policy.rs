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

/// 明细可查窗口的默认值（READ_STATUS_SPEC §6.5.4）。锚点是**消息发送时间**，
/// 不是"阅读之后再保留 N 天"。
pub const DEFAULT_READ_DETAIL_RETENTION_DAYS: i64 = 7;

/// 服务端决定的回执模式。
///
/// 🔴 **不从请求体解析。** 客户端声明不能替代服务端策略——外层验证了登录，不代表
/// 可以查任意消息的读者。这里先返回系统默认；接频道级/系统级配置时只改这一个函数。
pub fn resolve_read_receipt_mode() -> ReadReceiptMode {
    ReadReceiptMode::FullList
}

/// 明细保留期限（天）。留成函数是为了后面接系统配置时不必改调用点。
pub fn read_detail_retention_days() -> i64 {
    DEFAULT_READ_DETAIL_RETENTION_DAYS
}

/// 已读明细查询的**唯一**授权入口：人数与名单共用。
///
/// 逐条对应 READ_STATUS_SPEC §6.5.5：
/// 1. 请求者是该消息的发送者；
/// 2. 消息属于所声明的频道；
/// 3. 消息未撤回；
/// 4. 未超过 §6.5.4 的窗口。
///
/// 返回明细截止时间，交给调用方下发——客户端据此决定显不显示入口，不自己写死天数。
pub fn authorize_read_detail(
    requester_id: u64,
    message: &crate::model::message::Message,
    channel_id: u64,
) -> RpcResult<chrono::DateTime<chrono::Utc>> {
    if message.channel_id != channel_id {
        return Err(RpcError::validation(
            "message_id 与 channel_id 不匹配".to_string(),
        ));
    }
    // 只有发送者能看谁读了自己的消息。三家产品一致，没有争议。
    if message.sender_id != requester_id {
        return Err(RpcError::forbidden(
            "only the sender may query who read this message".to_string(),
        ));
    }
    if message.revoked || message.deleted {
        return Err(RpcError::not_found("message is revoked".to_string()));
    }
    let expires_at =
        message.created_at + chrono::Duration::days(read_detail_retention_days());
    if chrono::Utc::now() >= expires_at {
        // 过期要能和"无人已读"区分开：这里必须是错误，不能返回空列表。
        return Err(RpcError::forbidden(
            "read detail window expired".to_string(),
        ));
    }
    Ok(expires_at)
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

    use crate::model::message::Message;
    use privchat_protocol::ContentMessageType;

    fn msg(sender_id: u64, channel_id: u64, age_days: i64, revoked: bool) -> Message {
        Message {
            message_id: 1,
            channel_id,
            sender_id,
            pts: Some(10),
            local_message_id: None,
            content: String::new(),
            message_type: ContentMessageType::Text,
            metadata: serde_json::Value::Null,
            reply_to_message_id: None,
            created_at: chrono::Utc::now() - chrono::Duration::days(age_days),
            updated_at: chrono::Utc::now(),
            deleted: false,
            deleted_at: None,
            revoked,
            revoked_at: None,
            revoked_by: None,
        }
    }

    /// 只有发送者能看谁读了自己的消息（§6.5.5）。
    /// 这条挡的是「登录了就能查任意消息读者」——外层的身份校验不等于消息级授权。
    #[test]
    fn only_the_sender_may_query() {
        let m = msg(7, 100, 0, false);
        assert!(authorize_read_detail(7, &m, 100).is_ok());
        assert!(authorize_read_detail(8, &m, 100).is_err());
    }

    /// 窗口锚在**发送时间**，不是"读完再留 N 天"（§6.5.4）。
    #[test]
    fn the_window_is_anchored_to_the_send_time() {
        assert!(authorize_read_detail(7, &msg(7, 100, 6, false), 100).is_ok());
        assert!(authorize_read_detail(7, &msg(7, 100, 8, false), 100).is_err());
    }

    /// 过期必须是**错误**，不能退化成空名单——否则和"无人已读"分不开（§6.5.4）。
    #[test]
    fn expiry_is_an_error_not_an_empty_list() {
        let err = authorize_read_detail(7, &msg(7, 100, 30, false), 100).unwrap_err();
        assert!(format!("{:?}", err).contains("expired"));
    }

    #[test]
    fn revoked_and_mismatched_channel_are_rejected() {
        assert!(authorize_read_detail(7, &msg(7, 100, 0, true), 100).is_err());
        assert!(authorize_read_detail(7, &msg(7, 100, 0, false), 999).is_err());
    }

    /// 模式是服务端决定的，签名里没有请求体——这条防的是把它改回从 body 解析。
    #[test]
    fn the_mode_comes_from_the_server() {
        assert_eq!(resolve_read_receipt_mode(), ReadReceiptMode::FullList);
    }

    #[test]
    fn parse_mode_defaults_to_full_list() {
        assert_eq!(
            ReadReceiptMode::parse(None).expect("parse none"),
            ReadReceiptMode::FullList
        );
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
