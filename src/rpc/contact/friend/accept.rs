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

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::service::friend_service::AcceptFriendRequestResult;
use privchat_protocol::rpc::contact::friend::FriendAcceptRequest;
use privchat_protocol::ErrorCode;
use serde_json::{json, Value};

/// 处理 接受好友申请 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 接受好友申请 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: FriendAcceptRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request payload: {}", e)))?;

    // 从 ctx 填充 target_user_id
    request.target_user_id = crate::rpc::get_current_user_id(&ctx)?;

    let from_user_id = request.from_user_id;
    let user_id = request.target_user_id;

    // 先原子化处理好友申请，避免“预检查通过但后续已被并发消费”导致误判过期
    let already_friends = match services
        .friend_service
        .accept_friend_request_with_source(user_id, from_user_id)
        .await
    {
        Ok(AcceptFriendRequestResult::Accepted(_)) => false,
        Ok(AcceptFriendRequestResult::AlreadyFriends) => true,
        Err(crate::error::ServerError::NotFound(_)) => {
            return Err(RpcError::from_code(
                ErrorCode::FriendRequestExpired,
                ErrorCode::FriendRequestExpired.message().to_string(),
            ));
        }
        Err(e) => {
            tracing::error!(
                "❌ 接受好友申请失败: user_id={}, from_user_id={}, err={}",
                user_id,
                from_user_id,
                e
            );
            return Err(RpcError::internal(format!(
                "Accept friend request failed: {}",
                e
            )));
        }
    };

    // ✅ 使用数据库事务保证原子性
    // 执行顺序：
    // 1. 开启事务
    // 2. 在事务中创建会话（数据库操作）
    // 3. 提交事务
    // 4. 建立好友关系（内存操作）
    // 这样确保：如果会话创建失败，好友关系不会被建立

    let mut tx = services
        .channel_service
        .pool()
        .begin()
        .await
        .map_err(|e| RpcError::internal(format!("Begin transaction failed: {}", e)))?;

    // 在事务中创建会话和 Channel
    let channel_id =
        match create_channel_and_channel_tx(&mut tx, &services, user_id, from_user_id).await {
            Ok(id) => id,
            Err(e) => {
                // 回滚事务
                let _ = tx.rollback().await;
                tracing::error!("❌ 创建会话失败（事务已回滚）: {}", e);
                return Err(RpcError::internal(format!(
                    "Accept friend request failed: cannot create channel - {}",
                    e
                )));
            }
        };

    // 提交事务
    tx.commit().await.map_err(|e| {
        tracing::error!("❌ 提交事务失败: {}", e);
        RpcError::internal(format!("Commit transaction failed: {}", e))
    })?;

    tracing::debug!("✅ 会话创建成功（事务已提交）: channel_id={}", channel_id);

    if already_friends {
        tracing::debug!(
            "ℹ️ 接受请求时检测到已是好友: {} <-> {}",
            user_id,
            from_user_id
        );
    }

    tracing::debug!(
        "✅ 好友申请接受成功: {} <-> {}, channel_id: {}",
        user_id,
        from_user_id,
        channel_id
    );

    // 返回会话 ID
    Ok(json!(channel_id))
}

/// 在事务中创建会话和 Channel
async fn create_channel_and_channel_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    services: &RpcServiceContext,
    user_id: u64,
    from_user_id: u64,
) -> Result<u64, String> {
    // 检查是否已存在私聊会话
    let existing_id = check_existing_channel_tx(tx, user_id, from_user_id).await?;
    if existing_id > 0 {
        tracing::debug!("✅ 私聊会话已存在: {}", existing_id);
        return Ok(existing_id);
    }

    // 在事务中创建新的私聊会话
    let channel_id = create_channel_tx(tx, user_id, from_user_id).await?;
    tracing::debug!("✅ 私聊会话已在事务中创建: {}", channel_id);

    // 创建 Channel（内存操作，不需要事务）
    // 注意：如果 Channel 创建失败，会导致事务回滚
    match services
        .channel_service
        .create_private_chat_with_id(user_id, from_user_id, channel_id)
        .await
    {
        Ok(_) => {
            tracing::debug!("✅ 私聊频道已创建: {}", channel_id);
        }
        Err(e) => {
            tracing::warn!("⚠️ 创建私聊频道失败: {}，频道可能已存在", e);
            // Channel 可能已存在，不应该失败整个事务
        }
    }

    Ok(channel_id)
}

/// 检查会话是否已存在（在事务中）
async fn check_existing_channel_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user1_id: u64,
    user2_id: u64,
) -> Result<u64, String> {
    let (smaller_id, larger_id) = if user1_id < user2_id {
        (user1_id, user2_id)
    } else {
        (user2_id, user1_id)
    };

    let row = sqlx::query_as::<_, (Option<i64>,)>(
        r#"
        SELECT channel_id
        FROM privchat_channels
        WHERE channel_type = 0
          AND direct_user1_id = $1
          AND direct_user2_id = $2
        LIMIT 1
        "#,
    )
    .bind(smaller_id as i64)
    .bind(larger_id as i64)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(|e| format!("查询已存在会话失败: {}", e))?;

    Ok(row.and_then(|(id,)| id).map(|id| id as u64).unwrap_or(0))
}

/// 创建会话（在事务中）
async fn create_channel_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    creator_id: u64,
    target_user_id: u64,
) -> Result<u64, String> {
    let (user1_id, user2_id) = if creator_id < target_user_id {
        (creator_id, target_user_id)
    } else {
        (target_user_id, creator_id)
    };

    let now = chrono::Utc::now().timestamp_millis();

    let row = sqlx::query_as::<_, (i64,)>(
        r#"
        INSERT INTO privchat_channels (
            channel_type, direct_user1_id, direct_user2_id,
            group_id, last_message_id, last_message_at, message_count,
            created_at, updated_at
        )
        VALUES (0, $1, $2, NULL, NULL, NULL, 0, $3, $3)
        RETURNING channel_id
        "#,
    )
    .bind(user1_id as i64)
    .bind(user2_id as i64)
    .bind(now)
    .fetch_one(tx.as_mut())
    .await
    .map_err(|e| format!("创建会话失败: {}", e))?;

    let channel_id = row.0 as u64;

    // 添加会话参与者
    for user_id in [creator_id, target_user_id] {
        sqlx::query(
            r#"
            INSERT INTO privchat_channel_participants (
                channel_id, user_id, role, joined_at
            )
            VALUES ($1, $2, 0, $3)
            ON CONFLICT (channel_id, user_id) DO NOTHING
            "#,
        )
        .bind(channel_id as i64)
        .bind(user_id as i64)
        .bind(now)
        .execute(tx.as_mut())
        .await
        .map_err(|e| format!("添加会话参与者失败: {}", e))?;
    }

    Ok(channel_id)
}
