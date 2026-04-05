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
use crate::infra::{ConnectionManager, SubscribeManager};
use crate::Result;
use async_trait::async_trait;
use msgtrans::SessionId;
use std::sync::Arc;
use tracing::{info, warn};

const ACTION_SUBSCRIBE: u8 = 1;
const ACTION_UNSUBSCRIBE: u8 = 2;

pub struct SubscribeMessageHandler {
    subscribe_manager: Arc<SubscribeManager>,
    channel_service: Arc<crate::service::ChannelService>,
    connection_manager: Arc<ConnectionManager>,
    room_history_service: Arc<crate::service::RoomHistoryService>,
}

impl SubscribeMessageHandler {
    pub fn new(
        subscribe_manager: Arc<SubscribeManager>,
        channel_service: Arc<crate::service::ChannelService>,
        connection_manager: Arc<ConnectionManager>,
        room_history_service: Arc<crate::service::RoomHistoryService>,
    ) -> Self {
        Self {
            subscribe_manager,
            channel_service,
            connection_manager,
            room_history_service,
        }
    }

    async fn replay_room_history_to_session(
        connection_manager: Arc<ConnectionManager>,
        room_history_service: Arc<crate::service::RoomHistoryService>,
        session_id: SessionId,
        channel_id: u64,
    ) -> Result<()> {
        let history = room_history_service.list_recent_history(channel_id).await?;
        if history.is_empty() {
            return Ok(());
        }

        let transport = connection_manager.transport_server.read().await;
        let Some(server) = transport.as_ref() else {
            warn!(
                "⚠️ SubscribeMessageHandler: TransportServer 未就绪，跳过 Room 历史回放 channel_id={}, session={}",
                channel_id, session_id
            );
            return Ok(());
        };

        let mut delivered = 0usize;
        for publish_request in history {
            let payload_bytes = match privchat_protocol::encode_message(&publish_request) {
                Ok(bytes) => bytes,
                Err(e) => {
                    warn!(
                        "⚠️ SubscribeMessageHandler: 编码 Room 历史失败 channel_id={}, session={}, error={}",
                        channel_id, session_id, e
                    );
                    continue;
                }
            };

            let mut packet = msgtrans::packet::Packet::one_way(0u32, payload_bytes);
            packet.set_biz_type(privchat_protocol::protocol::MessageType::PublishRequest as u8);

            if let Err(e) = server.send_to_session(session_id.clone(), packet).await {
                warn!(
                    "⚠️ SubscribeMessageHandler: 回放 Room 历史失败 channel_id={}, session={}, error={}",
                    channel_id, session_id, e
                );
                continue;
            }

            delivered += 1;
        }

        info!(
            "📚 SubscribeMessageHandler: Room 历史回放完成 channel_id={}, session={}, delivered={}",
            channel_id, session_id, delivered
        );

        Ok(())
    }
}

#[async_trait]
impl MessageHandler for SubscribeMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!(
            "📡 SubscribeMessageHandler: 处理来自会话 {} 的订阅请求",
            context.session_id
        );

        let subscribe_request: privchat_protocol::protocol::SubscribeRequest =
            privchat_protocol::decode_message(&context.data).map_err(|e| {
                crate::error::ServerError::Protocol(format!("解码订阅请求失败: {}", e))
            })?;

        let channel_type = subscribe_request.channel_type;
        let action = subscribe_request.action;
        let channel_id = subscribe_request.channel_id;

        info!(
            "📡 SubscribeMessageHandler: 频道: {}, 类型: {}, 操作: {}",
            channel_id, channel_type, action
        );

        let mut reason_code = 0u8;
        let mut should_replay_room_history = false;

        // 授权检查（仅 subscribe 需要，unsubscribe 无条件放行）
        if action == ACTION_SUBSCRIBE {
            reason_code = match channel_type {
                // Private(0) / Group(1)：校验连接绑定的 user_id 是否为频道成员
                0 | 1 => {
                    match &context.user_id {
                        Some(uid_str) => {
                            if let Ok(uid) = uid_str.parse::<u64>() {
                                match self.channel_service.get_channel_members(&channel_id).await {
                                    Ok(members) => {
                                        if members.iter().any(|m| m.user_id == uid) {
                                            0 // 授权通过
                                        } else {
                                            warn!(
                                                "📡 SubscribeMessageHandler: user {} 不是频道 {} 的成员",
                                                uid, channel_id
                                            );
                                            4 // NOT_MEMBER
                                        }
                                    }
                                    Err(e) => {
                                        warn!(
                                            "📡 SubscribeMessageHandler: 查询频道 {} 成员失败: {}",
                                            channel_id, e
                                        );
                                        5 // 查询失败
                                    }
                                }
                            } else {
                                6 // 无效 user_id
                            }
                        }
                        None => {
                            warn!(
                                "📡 SubscribeMessageHandler: 未认证的会话 {}",
                                context.session_id
                            );
                            6 // 未认证
                        }
                    }
                }
                // Room(2)：最小 gate — 必须已认证
                // TODO 后续迭代实现完整 ticket 校验：
                // 1. 从 subscribe_request.param 提取 ticket（JWT）
                // 2. 验证签名（HMAC-SHA256）、过期时间
                // 3. 校验 channel_id 匹配、device_id 匹配
                // 4. 校验 Room 状态（OPEN/CLOSED）
                // reason_code: 4=TICKET_EXPIRED, 5=TICKET_INVALID, 6=ROOM_CLOSED, 7=NOT_ALLOWED
                2 => {
                    if context.user_id.is_none() {
                        warn!(
                            "📡 SubscribeMessageHandler: 未认证的会话 {} 尝试订阅 Room {}",
                            context.session_id, channel_id
                        );
                        6 // 未认证
                    } else {
                        0 // 已认证即放行（ticket 校验后续迭代）
                    }
                }
                // 未知 channel_type
                _ => {
                    warn!(
                        "📡 SubscribeMessageHandler: 不支持的 channel_type: {}",
                        channel_type
                    );
                    2 // UNSUPPORTED_CHANNEL_TYPE
                }
            };
        }

        // 授权通过后执行订阅/取消订阅
        if reason_code == 0 {
            match action {
                ACTION_SUBSCRIBE => {
                    match self
                        .subscribe_manager
                        .subscribe(context.session_id.clone(), channel_id)
                    {
                        Ok(()) => {
                            info!(
                                "📡 SubscribeMessageHandler: Session {} 订阅频道 {}",
                                context.session_id, channel_id
                            );
                            should_replay_room_history = channel_type == 2
                                && self.room_history_service.subscribe_history_enabled();
                        }
                        Err(e) => {
                            reason_code = if e == "too many subscriptions" { 8 } else { 3 };
                            warn!(
                                "📡 SubscribeMessageHandler: Session {} 订阅频道 {} 失败: {}",
                                context.session_id, channel_id, e
                            );
                        }
                    }
                }
                ACTION_UNSUBSCRIBE => {
                    self.subscribe_manager
                        .unsubscribe(&context.session_id, channel_id);
                    info!(
                        "📡 SubscribeMessageHandler: Session {} 取消订阅频道 {}",
                        context.session_id, channel_id
                    );
                }
                _ => {
                    reason_code = 1;
                    warn!("📡 SubscribeMessageHandler: 未知操作: {}", action);
                }
            }
        }

        if should_replay_room_history {
            let connection_manager = self.connection_manager.clone();
            let room_history_service = self.room_history_service.clone();
            let session_id = context.session_id.clone();
            tokio::spawn(async move {
                if let Err(e) = SubscribeMessageHandler::replay_room_history_to_session(
                    connection_manager,
                    room_history_service,
                    session_id.clone(),
                    channel_id,
                )
                .await
                {
                    warn!(
                        "⚠️ SubscribeMessageHandler: Room 历史回放任务失败 channel_id={}, session={}, error={}",
                        channel_id, session_id, e
                    );
                }
            });
        }

        let subscribe_response = privchat_protocol::protocol::SubscribeResponse {
            local_message_id: subscribe_request.local_message_id,
            channel_id: subscribe_request.channel_id,
            channel_type: subscribe_request.channel_type,
            action: subscribe_request.action,
            reason_code,
        };

        let response_bytes = privchat_protocol::encode_message(&subscribe_response)
            .map_err(|e| crate::error::ServerError::Protocol(format!("编码订阅响应失败: {}", e)))?;

        info!("✅ SubscribeMessageHandler: 订阅请求处理完成");

        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "SubscribeMessageHandler"
    }
}
