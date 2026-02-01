use async_trait::async_trait;
use crate::context::RequestContext;
use crate::Result;

/// 消息处理器 trait
#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>>;
    fn name(&self) -> &'static str;
}

// 导出所有处理器
pub mod connect_message_handler;
pub mod disconnect_message_handler;
pub mod ping_message_handler;
pub mod send_message_handler;
pub mod subscribe_message_handler;
pub mod rpc_message_handler;

pub use connect_message_handler::ConnectMessageHandler;
pub use disconnect_message_handler::DisconnectMessageHandler;
pub use ping_message_handler::PingMessageHandler;
pub use send_message_handler::SendMessageHandler;
pub use subscribe_message_handler::SubscribeMessageHandler;
pub use rpc_message_handler::RPCMessageHandler; 