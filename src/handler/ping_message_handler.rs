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
use async_trait::async_trait;
use tracing::debug;

/// Ping消息处理器
pub struct PingMessageHandler;

impl PingMessageHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MessageHandler for PingMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        debug!(
            "🏓 PingMessageHandler: 处理来自会话 {} 的Ping请求",
            context.session_id
        );

        // 解析Ping请求
        let _ping_request: privchat_protocol::protocol::PingRequest =
            privchat_protocol::decode_message(&context.data).map_err(|e| {
                crate::error::ServerError::Protocol(format!("解码Ping请求失败: {}", e))
            })?;

        // 创建Pong响应
        let pong_response = privchat_protocol::protocol::PongResponse {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };

        // 编码响应
        let response_bytes = privchat_protocol::encode_message(&pong_response)
            .map_err(|e| crate::error::ServerError::Protocol(format!("编码Pong响应失败: {}", e)))?;

        debug!("✅ PingMessageHandler: Ping请求处理完成");

        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "PingMessageHandler"
    }
}
