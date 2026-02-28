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
use crate::Result;
use async_trait::async_trait;

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
pub mod rpc_message_handler;
pub mod send_message_handler;
pub mod subscribe_message_handler;

pub use connect_message_handler::ConnectMessageHandler;
pub use disconnect_message_handler::DisconnectMessageHandler;
pub use ping_message_handler::PingMessageHandler;
pub use rpc_message_handler::RPCMessageHandler;
pub use send_message_handler::SendMessageHandler;
pub use subscribe_message_handler::SubscribeMessageHandler;
