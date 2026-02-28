// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::context::RequestContext;
use crate::handler::MessageHandler;
use crate::Result;
use privchat_protocol::protocol::MessageType;
use std::collections::HashMap;
use tracing::warn;

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

    pub fn register_handler(
        &mut self,
        message_type: MessageType,
        handler: Box<dyn MessageHandler>,
    ) {
        self.handlers.insert(message_type, handler);
    }

    pub async fn dispatch(
        &self,
        message_type: MessageType,
        context: RequestContext,
    ) -> Result<Option<Vec<u8>>> {
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

    pub fn with_handler(
        mut self,
        message_type: MessageType,
        handler: Box<dyn MessageHandler>,
    ) -> Self {
        self.handlers.insert(message_type, handler);
        self
    }

    pub fn build(self) -> MessageDispatcher {
        MessageDispatcher {
            handlers: self.handlers,
        }
    }
}
