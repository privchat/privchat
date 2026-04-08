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
use crate::infra::SubscribeManager;
use crate::service::PresenceService;
use crate::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

pub struct DisconnectMessageHandler {
    connection_manager: Arc<crate::infra::ConnectionManager>,
    subscribe_manager: Arc<SubscribeManager>,
    presence_service: Arc<PresenceService>,
}

impl DisconnectMessageHandler {
    pub fn new(
        connection_manager: Arc<crate::infra::ConnectionManager>,
        subscribe_manager: Arc<SubscribeManager>,
        presence_service: Arc<PresenceService>,
    ) -> Self {
        Self {
            connection_manager,
            subscribe_manager,
            presence_service,
        }
    }
}

#[async_trait]
impl MessageHandler for DisconnectMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!(
            "🔌 DisconnectMessageHandler: 处理来自会话 {} 的断开连接请求",
            context.session_id
        );

        // 解析断开连接请求
        let disconnect_request: privchat_protocol::protocol::DisconnectRequest =
            privchat_protocol::decode_message(&context.data).map_err(|e| {
                crate::error::ServerError::Protocol(format!("解码断开连接请求失败: {}", e))
            })?;

        info!(
            "🔌 DisconnectMessageHandler: 用户请求断开连接，原因: {:?}",
            disconnect_request.reason
        );

        let disconnected_connection = self
            .connection_manager
            .get_connection_by_session(&context.session_id)
            .await;

        // 注销连接
        if let Err(e) = self
            .connection_manager
            .unregister_connection(context.session_id.clone())
            .await
        {
            warn!("⚠️ DisconnectMessageHandler: 注销连接失败: {}", e);
        } else {
            info!("✅ DisconnectMessageHandler: 连接已注销");
        }

        if let Some(connection) = disconnected_connection {
            if let Err(e) = self
                .presence_service
                .on_device_disconnected(connection.user_id, &connection.device_id)
                .await
            {
                warn!("⚠️ DisconnectMessageHandler: 更新 Presence 下线失败: {}", e);
            }
        }

        // 清理频道订阅
        let left_channels = self
            .subscribe_manager
            .on_session_disconnect(&context.session_id);
        if !left_channels.is_empty() {
            info!(
                "🔌 DisconnectMessageHandler: Session {} 离开频道 {:?}",
                context.session_id, left_channels
            );
        }

        // 创建断开连接响应
        let disconnect_response =
            privchat_protocol::protocol::DisconnectResponse { acknowledged: true };

        // 编码响应
        let response_bytes =
            privchat_protocol::encode_message(&disconnect_response).map_err(|e| {
                crate::error::ServerError::Protocol(format!("编码断开连接响应失败: {}", e))
            })?;

        info!("✅ DisconnectMessageHandler: 断开连接请求处理完成");

        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "DisconnectMessageHandler"
    }
}
