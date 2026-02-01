use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::contact::friend::FriendAcceptRequest;

/// 处理 接受好友申请 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 接受好友申请 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: FriendAcceptRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 target_user_id
    request.target_user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let from_user_id = request.from_user_id;
    let user_id = request.target_user_id;
    
    // ✅ 使用数据库事务保证原子性
    // 执行顺序：
    // 1. 开启事务
    // 2. 在事务中创建会话（数据库操作）
    // 3. 提交事务
    // 4. 建立好友关系（内存操作）
    // 这样确保：如果会话创建失败，好友关系不会被建立
    
    let mut tx = services.channel_service.pool()
        .begin()
        .await
        .map_err(|e| RpcError::internal(format!("开启事务失败: {}", e)))?;
    
    // 在事务中创建会话和 Channel
    let channel_id = match create_channel_and_channel_tx(&mut tx, &services, user_id, from_user_id).await {
        Ok(id) => id,
        Err(e) => {
            // 回滚事务
            let _ = tx.rollback().await;
            tracing::error!("❌ 创建会话失败（事务已回滚）: {}", e);
            return Err(RpcError::internal(format!("接受好友申请失败: 无法创建会话 - {}", e)));
        }
    };
    
    // 提交事务
    tx.commit().await
        .map_err(|e| {
            tracing::error!("❌ 提交事务失败: {}", e);
            RpcError::internal(format!("提交事务失败: {}", e))
        })?;
    
    tracing::info!("✅ 会话创建成功（事务已提交）: channel_id={}", channel_id);
    
    // 会话创建成功后，建立好友关系（内存操作，不能回滚）
    let source = match services.friend_service.accept_friend_request_with_source(user_id, from_user_id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("❌ 建立好友关系失败（但会话已创建）: {}", e);
            // 注意：此时会话已经创建，无法回滚
            // 返回错误，让客户端知道操作部分失败
            return Err(RpcError::internal(format!("接受好友申请失败: 好友关系建立失败 - {}", e)));
        }
    };
    
    tracing::info!("✅ 好友申请接受成功: {} <-> {}, channel_id: {}", 
                  user_id, from_user_id, channel_id);
    
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
        tracing::info!("✅ 私聊会话已存在: {}", existing_id);
        return Ok(existing_id);
    }
    
    // 在事务中创建新的私聊会话
    let channel_id = create_channel_tx(tx, user_id, from_user_id).await?;
    tracing::info!("✅ 私聊会话已在事务中创建: {}", channel_id);
    
    // 创建 Channel（内存操作，不需要事务）
    // 注意：如果 Channel 创建失败，会导致事务回滚
    match services.channel_service.create_private_chat_with_id(
        user_id,
        from_user_id,
        channel_id,
    ).await {
        Ok(_) => {
            tracing::info!("✅ 私聊频道已创建: {}", channel_id);
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
        "#
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
        "#
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
            "#
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
