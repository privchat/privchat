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

use chrono::Utc;
use privchat_server::error::ServerError;
use privchat_server::infra::cache::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    println!("🚀 PrivChat 缓存系统演示");
    println!("基于 L1 (Moka) + L2 (Redis) 的分层缓存架构");
    println!();

    // 示例 1: 本地缓存演示
    println!("📦 示例 1: 本地缓存 (L1 Only)");
    demo_local_cache().await?;
    println!();

    // 示例 2: Redis 缓存演示
    #[cfg(feature = "redis")]
    {
        println!("📦 示例 2: Redis 分布式缓存 (L1 + L2)");
        demo_redis_cache().await?;
        println!();
    }

    // 示例 3: 业务缓存服务演示
    println!("📦 示例 3: 业务缓存服务演示");
    demo_business_cache().await?;
    println!();

    // 示例 4: 批量操作演示
    println!("📦 示例 4: 批量操作演示");
    demo_batch_operations().await?;
    println!();

    // 示例 5: 缓存一致性演示
    println!("📦 示例 5: 缓存一致性演示");
    demo_cache_consistency().await?;

    println!("✅ 所有演示完成！");
    Ok(())
}

/// 演示本地缓存的使用
async fn demo_local_cache() -> Result<(), ServerError> {
    println!("创建本地缓存管理器...");
    let cache_manager = Arc::new(CacheManager::new());
    let cache_service = Arc::new(ChatCacheService::new(cache_manager.clone()));

    // 创建测试用户状态
    let user_id = Uuid::new_v4();
    let user_status = UserStatus {
        user_id,
        status: "online".to_string(),
        devices: vec![
            DeviceInfo {
                device_id: "device-001".to_string(),
                platform: "iOS".to_string(),
                session_id: "session-001".to_string(),
                last_active: Utc::now(),
            },
            DeviceInfo {
                device_id: "device-002".to_string(),
                platform: "Web".to_string(),
                session_id: "session-002".to_string(),
                last_active: Utc::now(),
            },
        ],
        last_active: Utc::now(),
    };

    println!("设置用户在线状态...");
    cache_service.set_user_status(user_status.clone()).await?;

    println!("获取用户在线状态...");
    if let Some(cached_status) = cache_service.get_user_status(&user_id).await? {
        println!(
            "✅ 缓存命中: 用户 {} 状态 {}, 设备数量: {}",
            cached_status.user_id,
            cached_status.status,
            cached_status.devices.len()
        );
    } else {
        println!("❌ 缓存未命中");
    }

    // 测试会话列表缓存
    println!("测试会话列表缓存...");
    let channel_list = ChannelList {
        user_id,
        channels: vec![
            ChannelInfo {
                channel_id: Uuid::new_v4(),
                channel_type: "direct".to_string(),
                title: Some("与张三的对话".to_string()),
                icon_url: None,
                last_message: Some("你好！".to_string()),
                last_message_at: Some(Utc::now()),
                unread_count: 3,
                is_pinned: false,
                is_muted: false,
            },
            ChannelInfo {
                channel_id: Uuid::new_v4(),
                channel_type: "group".to_string(),
                title: Some("工作群".to_string()),
                icon_url: None,
                last_message: Some("明天开会".to_string()),
                last_message_at: Some(Utc::now()),
                unread_count: 15,
                is_pinned: true,
                is_muted: false,
            },
        ],
        updated_at: Utc::now(),
    };

    cache_service
        .update_channel_list(channel_list.clone())
        .await?;

    if let Some(cached_list) = cache_service.get_channel_list(&user_id).await? {
        println!("✅ 会话列表缓存命中: {} 个会话", cached_list.channels.len());
        for conv in &cached_list.channels {
            println!(
                "  - {}: {} (未读: {})",
                conv.channel_type,
                conv.title.as_deref().unwrap_or("无标题"),
                conv.unread_count
            );
        }
    }

    Ok(())
}

/// 演示 Redis 缓存的使用
#[cfg(feature = "redis")]
async fn demo_redis_cache() -> Result<(), ServerError> {
    println!("创建 Redis 缓存管理器...");
    // 注意：实际使用时请设置正确的 Redis URL
    let redis_url = "redis://localhost:6379";

    match CacheManager::with_redis(redis_url) {
        Ok(cache_manager) => {
            let cache_service = Arc::new(ChatCacheService::new(Arc::new(cache_manager)));

            let user_id = Uuid::new_v4();
            let user_status = UserStatus {
                user_id,
                status: "online".to_string(),
                devices: vec![DeviceInfo {
                    device_id: "device-redis-001".to_string(),
                    platform: "Android".to_string(),
                    session_id: "session-redis-001".to_string(),
                    last_active: Utc::now(),
                }],
                last_active: Utc::now(),
            };

            println!("设置用户状态到 Redis...");
            cache_service.set_user_status(user_status.clone()).await?;

            println!("从 Redis 获取用户状态...");
            if let Some(cached_status) = cache_service.get_user_status(&user_id).await? {
                println!(
                    "✅ Redis 缓存命中: 用户 {} 状态 {}",
                    cached_status.user_id, cached_status.status
                );
            }

            // 测试群成员缓存
            let group_id = Uuid::new_v4();
            let group_members = GroupMembers {
                group_id,
                member_ids: [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()]
                    .iter()
                    .cloned()
                    .collect(),
                admin_ids: [Uuid::new_v4()].iter().cloned().collect(),
                owner_id: Some(Uuid::new_v4()),
                updated_at: Utc::now(),
            };

            println!("设置群成员列表到 Redis...");
            cache_service
                .update_group_members(group_members.clone())
                .await?;

            if let Some(cached_members) = cache_service.get_group_members(&group_id).await? {
                println!(
                    "✅ 群成员缓存命中: {} 个成员, {} 个管理员",
                    cached_members.member_ids.len(),
                    cached_members.admin_ids.len()
                );
            }

            println!("✅ Redis 缓存演示完成");
        }
        Err(e) => {
            println!("⚠️  Redis 连接失败: {}", e);
            println!("请确保 Redis 服务器正在运行，或跳过此演示");
        }
    }

    Ok(())
}

/// 演示业务缓存服务
async fn demo_business_cache() -> Result<(), ServerError> {
    println!("创建业务缓存服务...");
    let cache_manager = Arc::new(CacheManager::new());
    let cache_service = Arc::new(ChatCacheService::new(cache_manager.clone()));

    let user_id = Uuid::new_v4();
    let friend_id = Uuid::new_v4();
    let channel_id = Uuid::new_v4();

    // 设置好友关系
    let friend_relations = FriendRelations {
        user_id,
        friend_ids: [friend_id].iter().cloned().collect(),
        blocked_ids: HashSet::new(),
        updated_at: Utc::now(),
    };

    println!("设置好友关系...");
    cache_manager
        .friend_relations
        .put(user_id, friend_relations, 3600)
        .await;

    // 检查好友关系
    println!("检查好友关系...");
    let is_friend = cache_service.is_friend(&user_id, &friend_id).await?;
    println!(
        "✅ 好友关系检查: {}",
        if is_friend {
            "是好友"
        } else {
            "不是好友"
        }
    );

    // 设置未读消息数
    println!("设置未读消息数...");
    cache_service
        .update_unread_count(&user_id, &channel_id, 42)
        .await?;

    let unread_count = cache_service
        .get_unread_count(&user_id, &channel_id)
        .await?;
    println!("✅ 未读消息数: {}", unread_count);

    // 清理用户缓存
    println!("清理用户缓存...");
    cache_service.clear_user_cache(&user_id).await?;

    let unread_count_after_clear = cache_service
        .get_unread_count(&user_id, &channel_id)
        .await?;
    println!("✅ 清理后未读消息数: {}", unread_count_after_clear);

    Ok(())
}

/// 演示批量操作
async fn demo_batch_operations() -> Result<(), ServerError> {
    println!("创建批量操作演示...");
    let cache_manager = Arc::new(CacheManager::new());

    // 创建多个用户状态
    let user_statuses = vec![
        (
            Uuid::new_v4(),
            UserStatus {
                user_id: Uuid::new_v4(),
                status: "online".to_string(),
                devices: vec![],
                last_active: Utc::now(),
            },
        ),
        (
            Uuid::new_v4(),
            UserStatus {
                user_id: Uuid::new_v4(),
                status: "away".to_string(),
                devices: vec![],
                last_active: Utc::now(),
            },
        ),
        (
            Uuid::new_v4(),
            UserStatus {
                user_id: Uuid::new_v4(),
                status: "offline".to_string(),
                devices: vec![],
                last_active: Utc::now(),
            },
        ),
    ];

    println!("批量设置用户状态...");
    cache_manager
        .user_status
        .mput(user_statuses.clone(), 3600)
        .await;

    println!("批量获取用户状态...");
    let user_ids: Vec<Uuid> = user_statuses.iter().map(|(id, _)| *id).collect();
    let cached_statuses = cache_manager.user_status.mget(&user_ids).await;

    println!("✅ 批量获取结果:");
    for (i, status) in cached_statuses.iter().enumerate() {
        match status {
            Some(s) => println!("  用户 {}: {}", i + 1, s.status),
            None => println!("  用户 {}: 未找到", i + 1),
        }
    }

    Ok(())
}

/// 演示缓存一致性
async fn demo_cache_consistency() -> Result<(), ServerError> {
    println!("创建缓存一致性演示...");
    let cache_manager = Arc::new(CacheManager::new());
    let cache_service = Arc::new(ChatCacheService::new(cache_manager.clone()));

    let user_id = Uuid::new_v4();
    let initial_status = UserStatus {
        user_id,
        status: "online".to_string(),
        devices: vec![],
        last_active: Utc::now(),
    };

    println!("设置初始用户状态...");
    cache_service
        .set_user_status(initial_status.clone())
        .await?;

    println!("第一次获取用户状态...");
    if let Some(status) = cache_service.get_user_status(&user_id).await? {
        println!("✅ 状态: {}", status.status);
    }

    // 模拟状态变更
    println!("更新用户状态...");
    let updated_status = UserStatus {
        user_id,
        status: "away".to_string(),
        devices: vec![],
        last_active: Utc::now(),
    };
    cache_service
        .set_user_status(updated_status.clone())
        .await?;

    println!("再次获取用户状态...");
    if let Some(status) = cache_service.get_user_status(&user_id).await? {
        println!("✅ 更新后状态: {}", status.status);
    }

    // 测试缓存失效
    println!("手动使缓存失效...");
    cache_manager.user_status.invalidate(&user_id).await;

    println!("失效后获取用户状态...");
    if let Some(status) = cache_service.get_user_status(&user_id).await? {
        println!("✅ 失效后状态: {}", status.status);
    } else {
        println!("✅ 缓存已失效，未找到状态");
    }

    Ok(())
}

/// 演示缓存性能
async fn demo_cache_performance() -> Result<(), ServerError> {
    println!("创建缓存性能测试...");
    let cache_manager = Arc::new(CacheManager::new());
    let cache_service = Arc::new(ChatCacheService::new(cache_manager.clone()));

    let user_id = Uuid::new_v4();
    let user_status = UserStatus {
        user_id,
        status: "online".to_string(),
        devices: vec![],
        last_active: Utc::now(),
    };

    // 设置缓存
    cache_service.set_user_status(user_status.clone()).await?;

    // 性能测试
    println!("进行性能测试...");
    let start = std::time::Instant::now();

    for _ in 0..10000 {
        let _ = cache_service.get_user_status(&user_id).await?;
    }

    let duration = start.elapsed();
    println!("✅ 10000 次缓存读取耗时: {:?}", duration);
    println!("✅ 平均每次读取: {:?}", duration / 10000);

    Ok(())
}

/// 输出缓存架构信息
fn print_cache_architecture() {
    println!("🏗️  缓存架构说明:");
    println!();
    println!("               ┌────────────┐");
    println!("               │  Moka (L1) │ ←─ 本地高速缓存，低延迟，短TTL");
    println!("               └─────┬──────┘");
    println!("                     │");
    println!("                Miss │");
    println!("                     ↓");
    println!("              ┌────────────┐");
    println!("              │ Redis (L2) │ ←─ 共享分布式缓存，中TTL");
    println!("              └─────┬──────┘");
    println!("                    │");
    println!("               Miss │");
    println!("                    ↓");
    println!("              ┌────────────┐");
    println!("              │ Database   │ ←─ 永久存储，慢");
    println!("              └────────────┘");
    println!();
    println!("🚀 优势:");
    println!("• 热数据读 L1，低延迟，高并发");
    println!("• 一致性靠 Redis 控制（L2 有 TTL，来源于 DB 的回源层）");
    println!("• 支持缓存穿透、失效、延迟双删等机制");
    println!("• 完全由你控制生命周期和容错策略");
    println!();
}
