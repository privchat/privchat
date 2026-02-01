use privchat_server::model::db::*;
use privchat_server::repository::{
    DatabaseManager, PaginationParams, 
    channel_repo::ChannelRepository, 
    message_repo::MessageRepository
};
use privchat_server::error::DatabaseError;
use sqlx::PgPool;
use uuid::Uuid;

/// 数据库使用示例
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::init();
    
    // 连接数据库
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://username:password@localhost/privchat_db".to_string());
    
    let pool = PgPool::connect(&database_url).await?;
    
    // 创建数据库管理器
    let db_manager = DatabaseManager::new(pool.clone());
    
    // 运行迁移
    println!("Running database migrations...");
    db_manager.run_migrations().await?;
    
    // 检查连接
    println!("Checking database connection...");
    db_manager.check_connection().await?;
    
    // 创建 Repository 实例
    let channel_repo = ChannelRepository::new(pool.clone());
    let message_repo = MessageRepository::new(pool.clone());
    
    // 示例：创建用户
    let user1_id = Uuid::new_v4();
    let user2_id = Uuid::new_v4();
    
    println!("Creating direct channel between users...");
    
    // 创建单聊会话
    let channel = channel_repo
        .create_or_get_direct_channel(&user1_id, &user2_id)
        .await?;
    
    println!("Created channel: {:?}", channel);
    
    // 发送消息
    println!("Sending messages...");
    
    let message1 = DbMessage::new(
        channel.channel_id,
        user1_id,
        "Hello! 这是第一条消息".to_string(),
        DbMessageType::Text,
    );
    
    let created_message = message_repo.create_message(&message1).await?;
    println!("Created message: {:?}", created_message);
    
    let message2 = DbMessage::new(
        channel.channel_id,
        user2_id,
        "Hi! 收到了你的消息".to_string(),
        DbMessageType::Text,
    );
    
    let created_message2 = message_repo.create_message(&message2).await?;
    println!("Created message: {:?}", created_message2);
    
    // 获取会话消息
    println!("Fetching channel messages...");
    let pagination = PaginationParams::new(1, 10);
    let messages = message_repo
        .get_channel_messages(&channel.channel_id, &pagination)
        .await?;
    
    println!("Messages in channel: {:?}", messages);
    
    // 创建群聊示例
    println!("Creating group channel...");
    
    let group_channel = DbChannel::new(
        DbChannelType::Group,
        Some("测试群聊".to_string()),
        user1_id,
    );
    
    let created_group = channel_repo
        .create_channel(&group_channel)
        .await?;
    
    println!("Created group channel: {:?}", created_group);
    
    // 添加群成员
    let participant1 = DbChannelParticipant::new(
        created_group.channel_id,
        user1_id,
        DbParticipantRole::Owner,
    );
    
    let participant2 = DbChannelParticipant::new(
        created_group.channel_id,
        user2_id,
        DbParticipantRole::Member,
    );
    
    channel_repo.add_participant(&participant1).await?;
    channel_repo.add_participant(&participant2).await?;
    
    // 获取群成员
    let participants = channel_repo
        .get_participants(&created_group.channel_id)
        .await?;
    
    println!("Group participants: {:?}", participants);
    
    // 频道示例
    println!("Creating channel...");
    
    let channel = Channel::new("系统通知".to_string(), user1_id);
    
    // 这里需要 ChannelRepository，但为了简化示例，我们跳过
    println!("Channel would be created here");
    
    // 会话列表示例
    println!("Getting user channels...");
    
    let user_channels = channel_repo
        .find_user_channels(&user1_id, &pagination)
        .await?;
    
    println!("User channels: {:?}", user_channels);
    
    // 搜索消息示例
    println!("Searching messages...");
    
    let search_results = message_repo
        .search_messages(&channel.channel_id, "消息", &pagination)
        .await?;
    
    println!("Search results: {:?}", search_results);
    
    println!("Database usage example completed successfully!");
    
    Ok(())
}

/// 频道消息示例
async fn channel_message_example(pool: PgPool) -> Result<(), DatabaseError> {
    use privchat_server::repository::message_repo::ChannelMessageRepository;
    
    let channel_repo = ChannelMessageRepository::new(pool);
    let channel_id = Uuid::new_v4();
    
    // 创建频道消息
    let channel_message = ChannelMessage::new(
        channel_id,
        Some(Uuid::new_v4()),
        Some("重要通知".to_string()),
        "系统将于今晚10点维护，请提前保存工作".to_string(),
        DbMessageType::Text,
    );
    
    let created_message = channel_repo.create_channel_message(&channel_message).await?;
    println!("Created channel message: {:?}", created_message);
    
    // 获取频道消息
    let pagination = PaginationParams::new(1, 10);
    let messages = channel_repo
        .get_channel_messages(&channel_id, &pagination)
        .await?;
    
    println!("Channel messages: {:?}", messages);
    
    Ok(())
}

/// 消息状态示例
async fn message_status_example(pool: PgPool) -> Result<(), DatabaseError> {
    use privchat_server::repository::message_repo::MessageStatusRepository;
    
    let status_repo = MessageStatusRepository::new(pool);
    let message_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let device_id = "device_001";
    
    // 标记消息为已读
    let success = status_repo
        .mark_message_as_read(&message_id, &user_id, device_id)
        .await?;
    
    println!("Message marked as read: {}", success);
    
    // 获取未读消息数量
    let channel_id = Uuid::new_v4();
    let unread_count = status_repo
        .get_unread_count(&channel_id, &user_id, device_id)
        .await?;
    
    println!("Unread messages count: {}", unread_count);
    
    Ok(())
}

/// 高级查询示例
async fn advanced_query_example(pool: PgPool) -> Result<(), DatabaseError> {
    let channel_repo = ChannelRepository::new(pool);
    
    // 获取活跃会话
    let pagination = PaginationParams::new(1, 20);
    let active_channels = channel_repo
        .get_active_channels(&pagination)
        .await?;
    
    println!("Active channels: {:?}", active_channels);
    
    Ok(())
} 