use std::sync::Arc;
use tokio::time::{sleep, Duration};

// 只测试核心的会话模型和服务
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt::init();

    println!("🚀 Phase 6 会话系统基础测试开始");

    // 测试会话模型创建
    test_channel_model().await;
    
    // 测试会话服务
    test_channel_service().await;

    println!("🎉 Phase 6 会话系统基础测试完成！");

    Ok(())
}

async fn test_channel_model() {
    println!("📋 测试1: 会话模型创建");

    // 使用完整的模块路径来避免导入问题
    use privchat_server::model::channel::{
        Channel, ChannelType, MemberRole
    };

    // 创建私聊会话
    let direct_conv = Channel::new_direct(
        "conv_direct_123".to_string(),
        "alice".to_string(),
        "bob".to_string(),
    );

    println!("✅ 私聊会话创建成功:");
    println!("   - ID: {}", direct_conv.id);
    println!("   - 类型: {:?}", direct_conv.channel_type);
    println!("   - 成员数: {}", direct_conv.members.len());
    println!("   - 成员列表: {:?}", direct_conv.get_member_ids());

    // 创建群聊会话
    let group_conv = Channel::new_group(
        "conv_group_456".to_string(),
        "alice".to_string(),
        Some("测试群聊".to_string()),
    );

    println!("✅ 群聊会话创建成功:");
    println!("   - ID: {}", group_conv.id);
    println!("   - 类型: {:?}", group_conv.channel_type);
    println!("   - 名称: {:?}", group_conv.metadata.name);
    println!("   - 成员数: {}", group_conv.members.len());
    println!("   - 创建者: {}", group_conv.creator_id);

    // 测试成员操作
    let mut test_group = group_conv.clone();
    match test_group.add_member("bob".to_string(), Some(MemberRole::Member)) {
        Ok(_) => println!("✅ 成员添加成功，新成员数: {}", test_group.members.len()),
        Err(e) => println!("❌ 成员添加失败: {}", e),
    }

    // 测试权限检查
    let can_send = test_group.check_permission("alice", |perms| perms.can_send_message);
    let can_manage = test_group.check_permission("alice", |perms| perms.can_manage_permissions);
    println!("✅ 权限检查 - alice 可发消息: {}, 可管理权限: {}", can_send, can_manage);

    println!("✅ 会话模型测试完成\n");
}

async fn test_channel_service() {
    println!("🔧 测试2: 会话服务功能");

    use privchat_server::service::channel_service::{
        ChannelService, ChannelServiceConfig
    };
    use privchat_server::model::channel::{
        ChannelType, CreateChannelRequest
    };

    // 创建服务配置
    let config = ChannelServiceConfig {
        max_channels_per_user: 100,
        max_group_members: 50,
        allow_public_groups: true,
        channel_id_prefix: "test_conv_".to_string(),
    };

    // 创建会话服务
    let service = Arc::new(ChannelService::new(config));

    // 创建私聊请求
    let direct_request = CreateChannelRequest {
        channel_type: ChannelType::Direct,
        name: None,
        description: None,
        member_ids: vec!["bob".to_string()],
        is_public: None,
        max_members: None,
    };

    // 测试创建私聊
    match service.create_channel("alice".to_string(), direct_request).await {
        Ok(response) => {
            if response.success {
                println!("✅ 私聊创建成功: {}", response.channel.id);
                println!("   - 成员数: {}", response.channel.members.len());
            } else {
                println!("❌ 私聊创建失败: {:?}", response.error);
            }
        }
        Err(e) => {
            println!("❌ 私聊创建出错: {}", e);
        }
    }

    // 创建群聊请求
    let group_request = CreateChannelRequest {
        channel_type: ChannelType::Group,
        name: Some("测试开发组".to_string()),
        description: Some("这是一个测试群聊".to_string()),
        member_ids: vec!["bob".to_string(), "charlie".to_string()],
        is_public: Some(false),
        max_members: Some(20),
    };

    // 测试创建群聊
    let mut test_group_id = String::new();
    match service.create_channel("alice".to_string(), group_request).await {
        Ok(response) => {
            if response.success {
                test_group_id = response.channel.id.clone();
                println!("✅ 群聊创建成功: {}", response.channel.id);
                println!("   - 名称: {:?}", response.channel.metadata.name);
                println!("   - 成员数: {}", response.channel.members.len());
                println!("   - 创建者: {}", response.channel.creator_id);
            } else {
                println!("❌ 群聊创建失败: {:?}", response.error);
            }
        }
        Err(e) => {
            println!("❌ 群聊创建出错: {}", e);
        }
    }

    // 测试加入群聊
    if !test_group_id.is_empty() {
        match service.join_channel(&test_group_id, "david".to_string(), None).await {
            Ok(success) => {
                if success {
                    println!("✅ david 成功加入群聊");
                } else {
                    println!("❌ david 加入群聊失败");
                }
            }
            Err(e) => {
                println!("❌ 加入群聊出错: {}", e);
            }
        }
    }

    // 测试获取用户会话列表
    let channels = service.get_user_channels("alice").await;
    println!("✅ alice 的会话列表:");
    println!("   - 总数: {}", channels.total);
    for (i, conv) in channels.channels.iter().enumerate() {
        println!("   {}. {} (类型: {:?}, 成员数: {})", 
                 i + 1, conv.id, conv.channel_type, conv.members.len());
    }

    // 测试查找私聊
    if let Some(direct_id) = service.find_direct_channel("alice", "bob").await {
        println!("✅ 找到 alice 和 bob 的私聊: {}", direct_id);
    } else {
        println!("❌ 未找到 alice 和 bob 的私聊");
    }

    // 测试统计信息
    let stats = service.get_stats().await;
    println!("✅ 会话服务统计:");
    println!("   - 总会话数: {}", stats.total_channels);
    println!("   - 活跃会话数: {}", stats.active_channels);
    println!("   - 私聊会话数: {}", stats.direct_channels);
    println!("   - 群聊会话数: {}", stats.group_channels);
    println!("   - 总成员数: {}", stats.total_members);
    println!("   - 平均每会话成员数: {:.2}", stats.avg_members_per_channel);

    println!("✅ 会话服务测试完成\n");
} 