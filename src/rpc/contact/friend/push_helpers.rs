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

//! `friend.request.*` 三类 inbox event push 的共用 helper。
//!
//! 所有 envelope payload 都是 `privchat-protocol` 里定义的结构体（**禁止手拼
//! JSON**）。每个 helper 内部构造 [`UserInboxEventEnvelope`] v1 + 包裹
//! [`PushMessageRequest`]，调用 [`ConnectionManager::send_push_to_user`]——
//! 后者已经在 user 维度对所有 authenticated session 做 fan-out（多端到达）。
//!
//! **不变式**（与 USER_INBOX_EVENT_ENVELOPE_SPEC §6/§8 一致）：
//! - envelope 是 online-push-only hint；不进 DB / 不进离线队列。
//! - SDK 收到后只触发 `SyncEntityChanged(entity_type="friend")`，不消费 payload。
//! - 权威数据来自 `entity/sync_entities("friend")` 拉取 friendships 表。

use crate::rpc::RpcServiceContext;
use privchat_protocol::inbox_event::{
    payloads::{
        FriendRequestReceivedPayload, FriendRequestSentPayload, FriendRequestStatusChangedPayload,
    },
    topics, UserInboxEventEnvelope,
};
use privchat_protocol::protocol::{MessageSetting, PushMessageRequest};
use privchat_protocol::ContentMessageType;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 构造一个 friend.request.* 类的 PushMessageRequest 外壳。
/// `topic` 决定事件类型，`from_uid` 是 envelope 来源（apply/accept/reject/recall
/// 的发起者）。
fn build_push_envelope(
    topic: &str,
    aggregate_id: u64,
    from_uid: u64,
    msg_key: String,
    payload_bytes: Vec<u8>,
) -> PushMessageRequest {
    let ts = now_ms();
    PushMessageRequest {
        setting: MessageSetting::default(),
        msg_key,
        server_message_id: 0,
        message_seq: 0,
        local_message_id: 0,
        stream_no: String::new(),
        stream_seq: 0,
        stream_flag: 0,
        timestamp: (ts / 1000) as u32,
        channel_id: 0,
        channel_type: 0,
        message_type: ContentMessageType::System.as_u32(),
        expire: 0,
        topic: topic.to_string(),
        from_uid,
        payload: payload_bytes,
        deleted: false,
    }
}

/// 推送 `friend.request.received` 到 target 的所有设备。
pub async fn push_friend_request_received(
    services: &RpcServiceContext,
    requester_id: u64,
    target_user_id: u64,
    message: &str,
) {
    let envelope = UserInboxEventEnvelope::new_v1(
        crate::infra::snowflake::next_message_id(),
        "friend_request",
        // 在 receiver 视角下 (self, requester) 唯一锁定 pending；receiver 由
        // push 通道隐含，aggregate_id 写 requester。
        requester_id.to_string(),
        now_ms(),
        None,
        serde_json::to_value(FriendRequestReceivedPayload {
            requester_id,
            receiver_id: target_user_id,
            message: message.to_string(),
        })
        .expect("FriendRequestReceivedPayload is always serializable"),
    );
    let payload_bytes = envelope
        .to_json_bytes()
        .expect("UserInboxEventEnvelope is always serializable");
    let push = build_push_envelope(
        topics::FRIEND_REQUEST_RECEIVED,
        requester_id,
        requester_id,
        format!("friend_received_{}_{}", requester_id, target_user_id),
        payload_bytes,
    );
    if let Err(e) = services
        .connection_manager
        .send_push_to_user(target_user_id, &push)
        .await
    {
        tracing::warn!(
            "⚠️ push friend.request.received failed target={}: {:?}",
            target_user_id,
            e
        );
    }
}

/// 推送 `friend.request.sent` 到 requester 自己的所有设备（多端同步）。
///
/// `send_push_to_user` 会 fan-out 到 requester 所有 session（包含本次发起 RPC
/// 的那个 session）；客户端按 envelope.event_id 做 dedupe，重复投递无害。
pub async fn push_friend_request_sent(
    services: &RpcServiceContext,
    requester_id: u64,
    target_user_id: u64,
) {
    let envelope = UserInboxEventEnvelope::new_v1(
        crate::infra::snowflake::next_message_id(),
        "friend_request",
        // requester 视角下 aggregate_id = target_user_id（"我发出的那条申请"）。
        target_user_id.to_string(),
        now_ms(),
        None,
        serde_json::to_value(FriendRequestSentPayload {
            requester_id,
            target_user_id,
        })
        .expect("FriendRequestSentPayload is always serializable"),
    );
    let payload_bytes = envelope
        .to_json_bytes()
        .expect("UserInboxEventEnvelope is always serializable");
    let push = build_push_envelope(
        topics::FRIEND_REQUEST_SENT,
        target_user_id,
        requester_id,
        format!("friend_sent_{}_{}", requester_id, target_user_id),
        payload_bytes,
    );
    if let Err(e) = services
        .connection_manager
        .send_push_to_user(requester_id, &push)
        .await
    {
        tracing::warn!(
            "⚠️ push friend.request.sent failed requester={}: {:?}",
            requester_id,
            e
        );
    }
}

/// 推送 `friend.request.status_changed` 到 **双方所有设备**。
///
/// 用于 accept / reject / recall（以及未来 GC expired）。`new_status` 直接传入
/// friendships.status 整型（1=accepted / 3=rejected / 4=recalled / 5=expired）。
pub async fn push_friend_request_status_changed(
    services: &RpcServiceContext,
    requester_id: u64,
    target_user_id: u64,
    new_status: i16,
    actor_user_id: u64,
) {
    push_friend_request_status_changed_via(
        &services.connection_manager,
        requester_id,
        target_user_id,
        new_status,
        actor_user_id,
    )
    .await
}

/// 与 [push_friend_request_status_changed] 等价，但只依赖 ConnectionManager ——
/// 供 service HTTP 路径（如 `/api/service/friendships` 直建好友）复用：
/// 建立后立刻通知双方在线设备，客户端无需重登即可看到新好友。
pub async fn push_friend_request_status_changed_via(
    connection_manager: &crate::infra::connection_manager::ConnectionManager,
    requester_id: u64,
    target_user_id: u64,
    new_status: i16,
    actor_user_id: u64,
) {
    let payload = serde_json::to_value(FriendRequestStatusChangedPayload {
        requester_id,
        target_user_id,
        new_status,
    })
    .expect("FriendRequestStatusChangedPayload is always serializable");

    // 给 requester 推一份
    {
        let envelope = UserInboxEventEnvelope::new_v1(
            crate::infra::snowflake::next_message_id(),
            "friend_request",
            // requester 视角下 aggregate_id = target_user_id
            target_user_id.to_string(),
            now_ms(),
            None,
            payload.clone(),
        );
        let payload_bytes = envelope
            .to_json_bytes()
            .expect("UserInboxEventEnvelope is always serializable");
        let push = build_push_envelope(
            topics::FRIEND_REQUEST_STATUS_CHANGED,
            target_user_id,
            actor_user_id,
            format!(
                "friend_sc_{}_{}_{}",
                requester_id, target_user_id, new_status
            ),
            payload_bytes,
        );
        if let Err(e) = connection_manager
            .send_push_to_user(requester_id, &push)
            .await
        {
            tracing::warn!(
                "⚠️ push friend.request.status_changed (requester={}) failed: {:?}",
                requester_id,
                e
            );
        }
    }

    // 给 target 推一份
    {
        let envelope = UserInboxEventEnvelope::new_v1(
            crate::infra::snowflake::next_message_id(),
            "friend_request",
            // target 视角下 aggregate_id = requester_id
            requester_id.to_string(),
            now_ms(),
            None,
            payload,
        );
        let payload_bytes = envelope
            .to_json_bytes()
            .expect("UserInboxEventEnvelope is always serializable");
        let push = build_push_envelope(
            topics::FRIEND_REQUEST_STATUS_CHANGED,
            requester_id,
            actor_user_id,
            format!(
                "friend_sc_{}_{}_{}",
                requester_id, target_user_id, new_status
            ),
            payload_bytes,
        );
        if let Err(e) = connection_manager
            .send_push_to_user(target_user_id, &push)
            .await
        {
            tracing::warn!(
                "⚠️ push friend.request.status_changed (target={}) failed: {:?}",
                target_user_id,
                e
            );
        }
    }
}
