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

use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

// 直接使用会话服务和处理器
use privchat_server::handler::channel_handler::ChannelHandler;
use privchat_server::model::channel::{ChannelType, CreateChannelRequest, MemberRole};
use privchat_server::service::channel_service::{ChannelService, ChannelServiceConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt::init();

    info!("🚀 Phase 6 会话系统核心演示开始");

    // 创建会话服务配置
    let config = ChannelServiceConfig {
        max_channels_per_user: 100,
        max_group_members: 50,
        allow_public_groups: true,
        channel_id_prefix: "conv_".to_string(),
    };

    // 创建会话服务
    let channel_service = Arc::new(ChannelService::new(config));
    let channel_handler = ChannelHandler::new(channel_service.clone());

    info!("✅ 会话服务初始化完成");

    // 演示1: 创建私聊会话
    info!("📱 演示1: 创建私聊会话");
    let direct_request = CreateChannelRequest {
        channel_type: ChannelType::Direct,
        name: None,
        description: None,
        member_ids: vec!["alice".to_string()],
        is_public: None,
        max_members: None,
    };

    match channel_handler
        .handle_create_channel("bob".to_string(), direct_request)
        .await
    {
        Ok(response) => {
            if response.success {
                info!("✅ 私聊会话创建成功: {}", response.channel.id);
                info!("   - 类型: {:?}", response.channel.channel_type);
                info!("   - 成员数: {}", response.channel.members.len());
                info!("   - 成员: {:?}", response.channel.get_member_ids());
            } else {
                warn!("❌ 私聊会话创建失败: {:?}", response.error);
            }
        }
        Err(e) => {
            warn!("❌ 私聊会话创建出错: {}", e);
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 演示2: 创建群聊会话
    info!("👥 演示2: 创建群聊会话");
    let group_request = CreateChannelRequest {
        channel_type: ChannelType::Group,
        name: Some("开发团队讨论组".to_string()),
        description: Some("日常开发讨论和技术分享".to_string()),
        member_ids: vec![
            "alice".to_string(),
            "charlie".to_string(),
            "david".to_string(),
        ],
        is_public: Some(false),
        max_members: Some(20),
    };

    let mut group_id = String::new();
    match channel_handler
        .handle_create_channel("bob".to_string(), group_request)
        .await
    {
        Ok(response) => {
            if response.success {
                group_id = response.channel.id.clone();
                info!("✅ 群聊会话创建成功: {}", response.channel.id);
                info!("   - 名称: {:?}", response.channel.metadata.name);
                info!("   - 描述: {:?}", response.channel.metadata.description);
                info!("   - 成员数: {}", response.channel.members.len());
                info!("   - 创建者: {}", response.channel.creator_id);
                info!(
                    "   - 最大成员数: {:?}",
                    response.channel.metadata.max_members
                );
            } else {
                warn!("❌ 群聊会话创建失败: {:?}", response.error);
            }
        }
        Err(e) => {
            warn!("❌ 群聊会话创建出错: {}", e);
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 演示3: 新成员加入群聊
    if !group_id.is_empty() {
        info!("🔄 演示3: 新成员加入群聊");
        match channel_handler
            .handle_join_channel("eve".to_string(), group_id.clone(), None)
            .await
        {
            Ok(response) => {
                if response.success {
                    info!("✅ 用户 eve 成功加入群聊");
                } else {
                    warn!("❌ 用户 eve 加入群聊失败: {:?}", response.error);
                }
            }
            Err(e) => {
                warn!("❌ 用户 eve 加入群聊出错: {}", e);
            }
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 演示4: 查找私聊会话
    info!("🔍 演示4: 查找私聊会话");
    match channel_handler
        .handle_find_direct_channel("bob".to_string(), "alice".to_string())
        .await
    {
        Ok(response) => {
            if response.found {
                info!("✅ 找到私聊会话: {:?}", response.channel_id);
            } else {
                warn!("❌ 未找到私聊会话");
            }
        }
        Err(e) => {
            warn!("❌ 查找私聊会话出错: {}", e);
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 演示5: 获取用户会话列表
    info!("📋 演示5: 获取用户会话列表");
    match channel_handler.handle_get_channels("bob".to_string()).await {
        Ok(response) => {
            info!("✅ 用户 bob 的会话列表:");
            info!("   - 总数: {}", response.total);
            for (i, conv) in response.channels.iter().enumerate() {
                info!(
                    "   {}. {} (类型: {:?}, 成员数: {})",
                    i + 1,
                    conv.id,
                    conv.channel_type,
                    conv.members.len()
                );
                if let Some(name) = &conv.metadata.name {
                    info!("      名称: {}", name);
                }
            }
        }
        Err(e) => {
            warn!("❌ 获取会话列表出错: {}", e);
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 演示6: 获取会话统计信息
    info!("📊 演示6: 获取会话统计信息");
    match channel_handler.handle_get_channel_stats().await {
        Ok(response) => {
            if response.success {
                let stats = &response.stats;
                info!("✅ 会话系统统计信息:");
                info!("   - 总会话数: {}", stats.total_channels);
                info!("   - 活跃会话数: {}", stats.active_channels);
                info!("   - 私聊会话数: {}", stats.direct_channels);
                info!("   - 群聊会话数: {}", stats.group_channels);
                info!("   - 系统会话数: {}", stats.system_channels);
                info!("   - 总成员数: {}", stats.total_members);
                info!(
                    "   - 平均每会话成员数: {:.2}",
                    stats.avg_members_per_channel
                );
            }
        }
        Err(e) => {
            warn!("❌ 获取统计信息出错: {}", e);
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 演示7: 成员离开群聊
    if !group_id.is_empty() {
        info!("🚪 演示7: 成员离开群聊");
        match channel_handler
            .handle_leave_channel("charlie".to_string(), group_id.clone())
            .await
        {
            Ok(response) => {
                if response.success {
                    info!("✅ 用户 charlie 成功离开群聊");
                } else {
                    warn!("❌ 用户 charlie 离开群聊失败: {:?}", response.error);
                }
            }
            Err(e) => {
                warn!("❌ 用户 charlie 离开群聊出错: {}", e);
            }
        }
    }

    sleep(Duration::from_millis(100)).await;

    // 最终统计
    info!("📈 最终统计信息:");
    let final_stats = channel_service.get_stats().await;
    info!("   - 总会话数: {}", final_stats.total_channels);
    info!("   - 活跃会话数: {}", final_stats.active_channels);
    info!("   - 私聊会话数: {}", final_stats.direct_channels);
    info!("   - 群聊会话数: {}", final_stats.group_channels);
    info!("   - 总成员数: {}", final_stats.total_members);
    info!(
        "   - 平均每会话成员数: {:.2}",
        final_stats.avg_members_per_channel
    );

    info!("🎉 Phase 6 会话系统核心演示完成！");

    // 演示核心特性总结
    info!("✨ Phase 6 会话系统核心特性:");
    info!("   1. 🔐 完整的会话模型 - 私聊、群聊、系统会话");
    info!("   2. 👥 灵活的成员管理 - 角色、权限、邀请、移除");
    info!("   3. 🎯 智能的会话索引 - 用户会话、私聊快速查找");
    info!("   4. 📊 实时统计监控 - 会话数量、成员分布、活跃状态");
    info!("   5. 🛡️ 权限控制系统 - 群主、管理员、成员、访客");
    info!("   6. 🔄 会话状态管理 - 活跃、归档、删除、封禁");
    info!("   7. 📱 消息关联支持 - 已读状态、最后消息追踪");
    info!("   8. 🧹 自动清理机制 - 无效会话检测和清理");

    Ok(())
}
