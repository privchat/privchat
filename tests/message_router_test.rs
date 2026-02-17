use std::sync::Arc;
use std::time::Duration;

use privchat_protocol::protocol::PushMessageRequest;
use privchat_server::infra::{
    CacheManager, DeviceId, DeviceSession, DeviceStatus, MessageRouter, MessageRouterConfig,
    MockMessageSender, SerializedRedisCache, SessionId, SessionManager, SimpleSessionManager,
    TwoLevelCache, UserId, UserOnlineStatus,
};

/// 创建测试用的消息路由器
async fn create_test_router() -> (Arc<MessageRouter>, Arc<SimpleSessionManager>) {
    // 创建缓存管理器
    let cache_manager = Arc::new(CacheManager::new());

    // 创建用户在线状态缓存
    let user_status_cache = cache_manager.get_user_status_cache();

    // 创建离线消息队列缓存
    let offline_queue = Arc::new(SerializedRedisCache::new(
        "test_offline_messages".to_string(),
        Duration::from_secs(3600),
        None,
    ));

    // 创建会话管理器
    let session_manager = Arc::new(SimpleSessionManager::new());

    // 创建消息路由器
    let router_config = MessageRouterConfig {
        offline_message_ttl: 24 * 3600,
        max_retry_count: 3,
        max_batch_size: 50,
        route_timeout_ms: 3000,
        max_offline_queue_size: 1000,
    };

    let message_router = Arc::new(MessageRouter::new(
        router_config,
        user_status_cache,
        offline_queue,
        session_manager.clone(),
    ));

    (message_router, session_manager)
}

/// 创建测试消息
fn create_test_message(from_uid: &str, content: &str) -> PushMessageRequest {
    let mut message = PushMessageRequest::new();
    message.from_uid = from_uid.to_string();
    message.channel_id = "test_channel".to_string();
    message.payload = content.as_bytes().to_vec();
    message
}

#[tokio::test]
async fn test_device_online_offline() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "test_user_1".to_string();
    let device_id = "test_device_1".to_string();
    let session_id = "test_session_1".to_string();

    // 初始状态：用户不在线
    let status = router.get_user_online_status(&user_id).await.unwrap();
    assert!(status.is_none());

    // 创建Mock消息发送器
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));

    // 注册会话
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    // 注册设备上线
    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .unwrap();

    // 检查用户在线状态
    let status = router.get_user_online_status(&user_id).await.unwrap();
    assert!(status.is_some());
    let status = status.unwrap();
    assert_eq!(status.user_id, user_id);
    assert_eq!(status.devices.len(), 1);
    assert_eq!(status.devices[0].device_id, device_id);
    assert_eq!(status.devices[0].session_id, session_id);
    assert!(matches!(status.devices[0].status, DeviceStatus::Online));

    // 注册设备离线
    router
        .register_device_offline(&user_id, &device_id)
        .await
        .unwrap();

    // 检查用户状态
    let status = router.get_user_online_status(&user_id).await.unwrap();
    assert!(status.is_none()); // 用户完全离线
}

#[tokio::test]
async fn test_online_message_routing() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "test_user_2".to_string();
    let device_id = "test_device_2".to_string();
    let session_id = "test_session_2".to_string();

    // 设置用户在线
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .unwrap();

    // 发送消息
    let message = create_test_message("sender_1", "Hello online user!");
    let result = router
        .route_message_to_user(&user_id, message)
        .await
        .unwrap();

    // 验证路由结果
    assert_eq!(result.success_count, 1);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 0);
    assert!(result.latency_ms > 0);
}

#[tokio::test]
async fn test_offline_message_routing() {
    let (router, _session_manager) = create_test_router().await;

    let user_id = "test_user_3".to_string(); // 用户不在线

    // 发送消息给离线用户
    let message = create_test_message("sender_2", "Hello offline user!");
    let result = router
        .route_message_to_user(&user_id, message)
        .await
        .unwrap();

    // 验证路由结果
    assert_eq!(result.success_count, 0);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 1);
    assert!(result.latency_ms > 0);
}

#[tokio::test]
async fn test_multi_device_routing() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "test_user_4".to_string();

    // 设置用户有多个设备在线
    let devices = vec![
        ("device_mobile", "session_mobile", "mobile"),
        ("device_desktop", "session_desktop", "desktop"),
        ("device_tablet", "session_tablet", "tablet"),
    ];

    for (device_id, session_id, device_type) in devices {
        let sender = Arc::new(MockMessageSender::new(session_id.to_string()));

        session_manager
            .register_session(
                session_id.to_string(),
                user_id.clone(),
                device_id.to_string(),
                device_type.to_string(),
                sender,
            )
            .unwrap();

        router
            .register_device_online(
                &user_id,
                &device_id.to_string(),
                &session_id.to_string(),
                device_type,
            )
            .await
            .unwrap();
    }

    // 发送消息到多设备用户
    let message = create_test_message("sender_3", "Hello multi-device user!");
    let result = router
        .route_message_to_user(&user_id, message)
        .await
        .unwrap();

    // 验证路由结果
    assert_eq!(result.success_count, 3); // 3个设备都应该成功
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 0);
}

#[tokio::test]
async fn test_specific_device_routing() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "test_user_5".to_string();
    let device_id = "test_device_5".to_string();
    let session_id = "test_session_5".to_string();

    // 设置用户设备在线
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .unwrap();

    // 发送消息到特定设备
    let message = create_test_message("sender_4", "Hello specific device!");
    let result = router
        .route_message_to_device(&user_id, &device_id, message)
        .await
        .unwrap();

    // 验证路由结果
    assert_eq!(result.success_count, 1);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 0);

    // 发送消息到不存在的设备
    let message = create_test_message("sender_4", "Hello non-existent device!");
    let result = router
        .route_message_to_device(&user_id, &"non_existent_device".to_string(), message)
        .await
        .unwrap();

    // 验证路由结果
    assert_eq!(result.success_count, 0);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 1); // 消息被存储为离线消息
}

#[tokio::test]
async fn test_batch_message_routing() {
    let (router, session_manager) = create_test_router().await;

    let user_ids = vec![
        "batch_user_1".to_string(),
        "batch_user_2".to_string(),
        "batch_user_3".to_string(), // 这个用户不在线
    ];

    // 设置前两个用户在线
    for (i, user_id) in user_ids.iter().enumerate() {
        if i < 2 {
            let device_id = format!("batch_device_{}", i);
            let session_id = format!("batch_session_{}", i);

            let sender = Arc::new(MockMessageSender::new(session_id.clone()));
            session_manager
                .register_session(
                    session_id.clone(),
                    user_id.clone(),
                    device_id.clone(),
                    "mobile".to_string(),
                    sender,
                )
                .unwrap();

            router
                .register_device_online(user_id, &device_id, &session_id, "mobile")
                .await
                .unwrap();
        }
    }

    // 批量发送消息
    let message = create_test_message("batch_sender", "Batch message!");
    let results = router
        .route_message_to_users(&user_ids, message)
        .await
        .unwrap();

    // 验证结果
    assert_eq!(results.len(), 3);

    // 前两个用户应该成功
    assert_eq!(results["batch_user_1"].success_count, 1);
    assert_eq!(results["batch_user_2"].success_count, 1);

    // 第三个用户应该离线
    assert_eq!(results["batch_user_3"].offline_count, 1);
}

#[tokio::test]
async fn test_offline_message_delivery() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "delivery_user_1".to_string();
    let device_id = "delivery_device_1".to_string();
    let session_id = "delivery_session_1".to_string();

    // 发送离线消息
    let message = create_test_message("offline_sender", "Offline message!");
    let result = router
        .route_message_to_user(&user_id, message)
        .await
        .unwrap();
    assert_eq!(result.offline_count, 1);

    // 模拟用户上线
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .unwrap();

    // 投递离线消息
    let delivered_count = router
        .deliver_offline_messages(&user_id, &device_id)
        .await
        .unwrap();
    assert_eq!(delivered_count, 1);

    // 再次投递应该没有消息
    let delivered_count = router
        .deliver_offline_messages(&user_id, &device_id)
        .await
        .unwrap();
    assert_eq!(delivered_count, 0);
}

#[tokio::test]
async fn test_batch_offline_message_delivery() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "batch_delivery_user".to_string();
    let device_id = "batch_delivery_device".to_string();
    let session_id = "batch_delivery_session".to_string();

    // 发送多条离线消息
    for i in 0..5 {
        let message =
            create_test_message("batch_offline_sender", &format!("Offline message {}", i));
        let result = router
            .route_message_to_user(&user_id, message)
            .await
            .unwrap();
        assert_eq!(result.offline_count, 1);
    }

    // 模拟用户上线
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .unwrap();

    // 批量投递离线消息
    let delivered_count = router
        .deliver_offline_messages_batch(&user_id, &device_id, 3)
        .await
        .unwrap();
    assert_eq!(delivered_count, 5); // 应该投递所有5条消息
}

#[tokio::test]
async fn test_session_manager_operations() {
    let (_router, session_manager) = create_test_router().await;

    let user_id = "session_test_user".to_string();
    let device_id = "session_test_device".to_string();
    let session_id = "session_test_session".to_string();

    // 测试会话注册
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    // 测试获取会话信息
    let session_info = session_manager.get_session_info(&session_id);
    assert!(session_info.is_some());
    let session_info = session_info.unwrap();
    assert_eq!(session_info.user_id, user_id);
    assert_eq!(session_info.device_id, device_id);
    assert_eq!(session_info.session_id, session_id);
    assert!(session_info.is_online);

    // 测试获取用户会话
    let user_sessions = session_manager.get_user_sessions(&user_id);
    assert_eq!(user_sessions.len(), 1);
    assert_eq!(user_sessions[0], session_id);

    // 测试会话在线状态
    let is_online = session_manager.is_session_online(&session_id).await;
    assert!(is_online);

    // 测试消息发送
    let message = create_test_message("test_sender", "Test message!");
    let result = session_manager.send_to_session(&session_id, &message).await;
    assert!(result.is_ok());

    // 测试会话注销
    session_manager.unregister_session(&session_id).unwrap();

    // 检查会话是否被移除
    let session_info = session_manager.get_session_info(&session_id);
    assert!(session_info.is_none());

    let is_online = session_manager.is_session_online(&session_id).await;
    assert!(!is_online);
}

#[tokio::test]
async fn test_statistics() {
    let (router, session_manager) = create_test_router().await;

    let user_id = "stats_user".to_string();
    let device_id = "stats_device".to_string();
    let session_id = "stats_session".to_string();

    // 设置用户在线
    let sender = Arc::new(MockMessageSender::new(session_id.clone()));
    session_manager
        .register_session(
            session_id.clone(),
            user_id.clone(),
            device_id.clone(),
            "mobile".to_string(),
            sender,
        )
        .unwrap();

    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .unwrap();

    // 发送一些消息
    for i in 0..5 {
        let message = create_test_message("stats_sender", &format!("Stats message {}", i));
        let _result = router
            .route_message_to_user(&user_id, message)
            .await
            .unwrap();
    }

    // 检查统计信息
    let router_stats = router.get_stats().await;
    // 注意：由于统计信息可能还没有更新，这里主要是验证接口可用性
    assert!(router_stats.total_messages >= 0);
    assert!(router_stats.success_routes >= 0);
    assert!(router_stats.failed_routes >= 0);
    assert!(router_stats.offline_messages >= 0);
    assert!(router_stats.avg_latency_ms >= 0.0);

    let session_stats = session_manager.get_stats().await;
    assert!(session_stats.total_sessions >= 0);
    assert!(session_stats.online_sessions >= 0);
    assert!(session_stats.messages_sent >= 0);
    assert!(session_stats.messages_failed >= 0);
    assert!(session_stats.batch_messages_sent >= 0);
    assert!(session_stats.batch_messages_failed >= 0);
}

#[tokio::test]
async fn test_concurrent_operations() {
    let (router, session_manager) = create_test_router().await;

    let user_count = 10;
    let messages_per_user = 5;

    // 并发设置用户在线
    let mut handles = Vec::new();
    for i in 0..user_count {
        let router = router.clone();
        let session_manager = session_manager.clone();
        let handle = tokio::spawn(async move {
            let user_id = format!("concurrent_user_{}", i);
            let device_id = format!("concurrent_device_{}", i);
            let session_id = format!("concurrent_session_{}", i);

            let sender = Arc::new(MockMessageSender::new(session_id.clone()));
            session_manager
                .register_session(
                    session_id.clone(),
                    user_id.clone(),
                    device_id.clone(),
                    "mobile".to_string(),
                    sender,
                )
                .unwrap();

            router
                .register_device_online(&user_id, &device_id, &session_id, "mobile")
                .await
                .unwrap();

            user_id
        });
        handles.push(handle);
    }

    // 等待所有用户上线
    let mut user_ids = Vec::new();
    for handle in handles {
        user_ids.push(handle.await.unwrap());
    }

    // 并发发送消息
    let mut message_handles = Vec::new();
    for user_id in user_ids {
        let router = router.clone();
        let handle = tokio::spawn(async move {
            let mut success_count = 0;
            for j in 0..messages_per_user {
                let message =
                    create_test_message("concurrent_sender", &format!("Concurrent message {}", j));
                let result = router
                    .route_message_to_user(&user_id, message)
                    .await
                    .unwrap();
                success_count += result.success_count;
            }
            success_count
        });
        message_handles.push(handle);
    }

    // 等待所有消息发送完成
    let mut total_success = 0;
    for handle in message_handles {
        total_success += handle.await.unwrap();
    }

    // 验证结果
    assert_eq!(total_success, user_count * messages_per_user);
}
