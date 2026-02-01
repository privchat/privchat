use std::collections::HashMap;
use crate::handler::MessageHandler;
use crate::context::RequestContext;
use crate::Result;
use tracing::warn;
use privchat_protocol::message::MessageType;

pub mod middleware;
pub use middleware::*;

/// 消息分发器
pub struct MessageDispatcher {
    handlers: HashMap<MessageType, Box<dyn MessageHandler>>,
}

impl MessageDispatcher {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register_handler(&mut self, message_type: MessageType, handler: Box<dyn MessageHandler>) {
        self.handlers.insert(message_type, handler);
    }

    pub async fn dispatch(&self, message_type: MessageType, context: RequestContext) -> Result<Option<Vec<u8>>> {
        // 查找处理器
        if let Some(handler) = self.handlers.get(&message_type) {
            handler.handle(context).await
        } else {
            warn!("未找到消息类型 {:?} 的处理器", message_type);
            Ok(None)
        }
    }
}

/// 消息分发器构建器
pub struct MessageDispatcherBuilder {
    handlers: HashMap<MessageType, Box<dyn MessageHandler>>,
}

impl MessageDispatcherBuilder {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn with_handler(mut self, message_type: MessageType, handler: Box<dyn MessageHandler>) -> Self {
        self.handlers.insert(message_type, handler);
        self
    }

    pub fn build(self) -> MessageDispatcher {
        MessageDispatcher {
            handlers: self.handlers,
        }
    }
} 