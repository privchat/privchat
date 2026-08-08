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

    // 读源消息的资格：转发人必须是源会话成员。
    if !services
        .channel_service
        .is_channel_member(source.channel_id, forwarder_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询源会话成员失败: {e}")))?
    {
        return Err(refusal(ForwardRefusal::SourceNotReadable));
    }

    // §6.3 内容保护：源会话禁止转发时，在创建副本**之前**拒绝。
    if let Some(group_id) = services
        .channel_service
        .get_channel_opt(source.channel_id)
        .await
        .and_then(|channel| channel.group_id)
    {
        let forbids = services
            .channel_service
            .get_group_policy(group_id)
            .await
            .map_err(|e| RpcError::internal(format!("查询群策略失败: {e}")))?
            .map(|policy| policy.forbid_forward)
            .unwrap_or(false);
        if forbids {
            return Err(refusal(ForwardRefusal::ForwardsRestricted));
        }
    }

    let target_channel = services
        .channel_service
        .get_channel_opt(request.target_channel_id)
        .await
        .ok_or_else(|| refusal(ForwardRefusal::TargetNotWritable))?;

    // 🔴 写入目标会话的资格走**与普通发送同一个**策略：禁言、全员禁言、角色权限、
    // 频道设置、私聊的好友/拉黑/隐私，一条不少。只查会话成员是不够的——
    // 被禁言或被拉黑的用户会从转发这条路把消息发出去。
    if let Err(send_refusal) = crate::service::send_authorization::authorize_send_to_channel(
        &crate::service::send_authorization::SendAuthorizationDeps {
            channel_service: services.channel_service.clone(),
            friend_service: services.friend_service.clone(),
            blacklist_service: services.blacklist_service.clone(),
            privacy_service: services.privacy_service.clone(),
        },
        &target_channel,
        forwarder_id,
    )
    .await
    {
        // 🔴 保留 typed 错误码：转发被禁言挡住和被拉黑挡住，客户端的处理不同
        // （一个是「等一会儿」，一个是「别再发了」）。全压成 PermissionDenied
        // 等于让转发比普通发送少一档信息。
        return Err(RpcError::from_code(
            send_refusal.error_code(),
            send_refusal.message(),
        ));
    }

    // 媒体引用由服务端复制，客户端一个 file_id 都没提交（§6 的安全前提）。
    let refs_from_table = services
        .message_repository
        .message_media_refs(request.source_message_id)
        .await
        .map_err(|e| RpcError::internal(format!("读取源消息媒体引用失败: {}", e)))?;
    // 事务内要拿这份快照复查源消息有没有在中途被改。
    let expected_source_refs: Vec<(u64, i16, i32)> = refs_from_table
        .iter()
        .map(|r| (r.file_id, r.role as i16, r.ordinal))
        .collect();
    let attachment_refs = refs_for_copy(
        refs_from_table,
        source.message_type as i32,
        &source.metadata,
    )
    .map_err(refusal)?;

    let source_origin = services
        .message_repository
        .forward_origin_of(request.source_message_id)
        .await
        .map_err(|e| RpcError::internal(format!("读取源消息转发来源失败: {}", e)))?;
    let mut forward_origin = root_origin_for_copy(
        source.message_id,
        source.channel_id,
        source.sender_id,
        source_origin,
    );
    // 作者名做成快照：接收方未必有权读源会话，事后查不到就只能显示 uid。
    if forward_origin.display_snapshot.is_none() {
        if let Ok(Some(user)) = services
            .user_service
            .find_by_id(forward_origin.root_author_id)
            .await
        {
            if let Some(name) = user.display_name.or(user.username) {
                forward_origin.display_snapshot =
                    Some(serde_json::json!({ "root_author_name": name }));
            }
        }
    }

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
            forward_precondition: Some(
                crate::repository::message_repo::ForwardPrecondition {
                    source_message_id: request.source_message_id,
                    expected_content: source.content.clone(),
                    expected_metadata: source.metadata.clone(),
                    expected_refs: expected_source_refs,
                },
            ),
        })
        .await
        .map_err(|e| {
            let text = e.to_string();
            if text.contains("FORWARD_SOURCE_GONE") {
                refusal(ForwardRefusal::SourceGone)
            } else if text.contains("FORWARD_SOURCE_NOT_FOUND") {
                refusal(ForwardRefusal::SourceNotFound)
            } else if text.contains("FORWARD_SOURCE_CHANGED") {
                refusal(ForwardRefusal::SourceChanged)
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
