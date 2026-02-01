// Notification - 通知模型占位符
// 
// 注意：Notification 类型已移除，统一使用 System 类型
// 系统消息（System）包括系统通知、群组操作等

// 重新导出 MessageType 中的 System 变体（替代原来的 Notification）
pub use super::MessageType::System; 