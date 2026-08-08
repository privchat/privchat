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

//! `message/forward` —— 单条转发（MEDIA_REFERENCE_AND_FORWARD_SPEC §6）。

use privchat_protocol::message::ContentMessageType;
use privchat_protocol::rpc::message::forward::{MessageForwardRequest, MessageForwardResponse};
use serde_json::Value;

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{RpcContext, RpcServiceContext};
use crate::service::forward_service::{
    is_forwardable, refs_for_copy, root_origin_for_copy, ForwardRefusal, FORWARD_ATTACHMENT_ORIGIN,
};

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    // 转发人取自认证连接。客户端传什么身份都不看（spec §6 第 1 步）。
    let forwarder_id = crate::rpc::get_current_user_id(&ctx)?;

    let request: MessageForwardRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    if request.client_request_id.trim().is_empty() {
        return Err(RpcError::validation("client_request_id 不能为空".to_string()));
    }

    let source = services
        .message_repository
        .get_message_for_forward(request.source_message_id)
        .await
        .map_err(|e| RpcError::internal(format!("读取源消息失败: {}", e)))?
        .ok_or_else(|| refusal(ForwardRefusal::SourceNotFound))?;

    if source.deleted || source.revoked {
        return Err(refusal(ForwardRefusal::SourceGone));
    }
    if source.channel_id != request.source_channel_id {
        // 客户端说错了源会话：要么客户端状态陈旧，要么在试探别的会话。
        return Err(refusal(ForwardRefusal::SourceChannelMismatch));
    }

    let source_type = u32::try_from(source.message_type)
        .ok()
        .and_then(ContentMessageType::from_u32)
        .ok_or_else(|| refusal(ForwardRefusal::TypeNotAllowed(ContentMessageType::Text)))?;
    if !is_forwardable(source_type) {
        return Err(refusal(ForwardRefusal::TypeNotAllowed(source_type)));
    }

    // 转发人必须读得到源消息，也必须写得进目标会话。两边都按会话成员判定。
    if !services
        .channel_service
        .is_channel_member(source.channel_id, forwarder_id)
        .await
        .unwrap_or(false)
    {
        return Err(refusal(ForwardRefusal::SourceNotReadable));
    }
    if !services
        .channel_service
        .is_channel_member(request.target_channel_id, forwarder_id)
        .await
        .unwrap_or(false)
    {
        return Err(refusal(ForwardRefusal::TargetNotWritable));
    }

    // 媒体引用由服务端复制，客户端一个 file_id 都没提交（§6 的安全前提）。
    let refs_from_table = services
        .message_repository
        .message_media_refs(request.source_message_id)
        .await
        .map_err(|e| RpcError::internal(format!("读取源消息媒体引用失败: {}", e)))?;
    let attachment_refs = refs_for_copy(
        refs_from_table,
        source.message_type as i32,
        &source.metadata,
    );

    let source_origin = services
        .message_repository
        .forward_origin_of(request.source_message_id)
        .await
        .map_err(|e| RpcError::internal(format!("读取源消息转发来源失败: {}", e)))?;
    let forward_origin = root_origin_for_copy(
        source.message_id,
        source.channel_id,
        source.sender_id,
        source_origin,
    );

    let target_channel = services
        .channel_service
        .get_channel_opt(request.target_channel_id)
        .await
        .ok_or_else(|| refusal(ForwardRefusal::TargetNotWritable))?;
    let recipient_user_ids: Vec<u64> = target_channel.members.keys().copied().collect();

    // 幂等键的作用域是 (uid, device, client_request_id)。做成全局键会让两个账号
    // 用同一个 request id 时互相判重——后一个人的转发静默变成前一个人的消息。
    let device_id = ctx.device_id.clone().unwrap_or_default();
    let dedup_key = format!(
        "forward:{forwarder_id}:{device_id}:{}",
        request.client_request_id
    );

    let result = services
        .message_service
        .send_message(crate::service::ServerSendMessageRequest {
            channel_id: request.target_channel_id,
            sender_id: forwarder_id,
            content: source.content.clone(),
            message_type: source_type,
            metadata: source.metadata.clone(),
            channel_type: match target_channel.channel_type {
                crate::model::channel::ChannelType::Direct => 1,
                crate::model::channel::ChannelType::Group => 2,
                crate::model::channel::ChannelType::Room => 3,
            },
            recipient_user_ids,
            dedup_key: Some(dedup_key),
            attachment_origin: FORWARD_ATTACHMENT_ORIGIN,
            attachment_refs_override: Some(attachment_refs),
            forward_origin: Some(forward_origin),
            require_live_source_message: Some(request.source_message_id),
        })
        .await
        .map_err(|e| {
            let text = e.to_string();
            if text.contains("FORWARD_SOURCE_GONE") {
                refusal(ForwardRefusal::SourceGone)
            } else if text.contains("FORWARD_SOURCE_NOT_FOUND") {
                refusal(ForwardRefusal::SourceNotFound)
            } else {
                RpcError::internal(format!("转发失败: {text}"))
            }
        })?;

    let response = MessageForwardResponse {
        message_id: result.message_id,
        channel_id: request.target_channel_id,
        pts: result.pts,
        created_at: result.created_at,
        deduplicated: !result.inserted,
    };
    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化失败: {}", e)))
}

fn refusal(reason: ForwardRefusal) -> RpcError {
    RpcError::forbidden(reason.code().to_string())
}
