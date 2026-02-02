// Notification - 通知模型占位符
//
// 注意：Notification 类型已移除，统一使用 System 类型
// 系统消息（System）包括系统通知、群组操作等
// 消息类型统一使用 protocol 层 ContentMessageType
pub use privchat_protocol::ContentMessageType::System; 