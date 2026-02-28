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

use privchat_server::middleware::SecurityMiddleware;
use privchat_server::security::{SecurityConfig, SecurityMode, SecurityService};
/// 安全系统集成示例
///
/// 展示如何在实际项目中集成和使用安全防护体系
/// 以及如何使用不同的安全模式
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔐 安全系统集成示例\n");

    // 演示三个阶段的配置
    demo_observe_mode().await?;
    demo_enforce_light_mode().await?;
    demo_enforce_full_mode().await?;

    Ok(())
}

/// 演示：ObserveOnly 模式（早期推荐）
async fn demo_observe_mode() -> Result<(), Box<dyn std::error::Error>> {
    println!("========================================");
    println!("🐣 阶段 1: ObserveOnly 模式");
    println!("========================================\n");

    // 1. 创建早期阶段配置
    let security_config = SecurityConfig::early_stage();
    let security_service = Arc::new(SecurityService::new(security_config));
    let security_middleware = Arc::new(SecurityMiddleware::new(security_service.clone()));

    println!("✅ 安全系统已初始化（ObserveOnly 模式）");
    println!("   - 只记录，不处罚");
    println!("   - 适合项目早期（< 1万用户）\n");

    // ============================================
    // 场景 1: 连接层检查
    // ============================================
    println!("\n📡 场景 1: 连接层检查");

    // 正常连接
    match security_middleware.check_connection("192.168.1.100").await {
        Ok(_) => println!("✅ IP 192.168.1.100 连接允许"),
        Err(e) => println!("❌ 连接被拒绝: {:?}", e),
    }

    // 模拟高频连接（触发限流）
    println!("\n模拟高频连接攻击...");
    for i in 0..20 {
        match security_middleware.check_connection("192.168.1.200").await {
            Ok(_) => println!("  第 {} 次连接: 允许", i + 1),
            Err(_) => {
                println!("  第 {} 次连接: 被限流（触发 IP 保护）", i + 1);
                break;
            }
        }
    }

    // ============================================
    // 场景 2: 消息发送检查（考虑 fan-out）
    // ============================================
    println!("\n💬 场景 2: 消息发送检查");

    let user_id = 1001;
    let device_id = "device_1";

    // 私聊（fan-out 小）
    let result = security_middleware
        .check_send_message(
            user_id, device_id, 2001,  // channel_id
            2,     // 2个人的私聊
            100,   // 100 bytes
            false, // 非媒体
        )
        .await?;

    println!(
        "✅ 私聊消息: 允许发送 (should_silent_drop: {})",
        result.should_silent_drop
    );

    // 大群（fan-out 大）
    let result = security_middleware
        .check_send_message(
            user_id, device_id, 2002, // channel_id
            500,  // 500人的大群
            100,  // 100 bytes
            false,
        )
        .await?;

    println!("✅ 大群消息: 允许发送 (成本更高，信任分消耗更多)");

    // 模拟高频发送（触发限流）
    println!("\n模拟高频发送...");
    for i in 0..50 {
        match security_middleware
            .check_send_message(user_id, device_id, 2003, 10, 100, false)
            .await
        {
            Ok(result) => {
                if result.should_silent_drop {
                    println!("  第 {} 条消息: Shadow Ban 触发！", i + 1);
                    break;
                }
            }
            Err(_) => {
                println!("  第 {} 条消息: 被限流", i + 1);
                break;
            }
        }
    }

    // ============================================
    // 场景 3: RPC 调用检查（基于成本）
    // ============================================
    println!("\n🔧 场景 3: RPC 调用检查");

    // 轻量 RPC
    let result = security_middleware
        .check_rpc(user_id, device_id, "users.getProfile", None)
        .await?;
    println!("✅ users.getProfile: 允许 (成本低)");

    // 重型 RPC
    let result = security_middleware
        .check_rpc(user_id, device_id, "sync.getFullState", None)
        .await?;
    println!("✅ sync.getFullState: 允许 (成本高，消耗更多令牌)");

    // 模拟高频 RPC（触发限流）
    println!("\n模拟高频 RPC 调用...");
    for i in 0..200 {
        match security_middleware
            .check_rpc(user_id, device_id, "users.getProfile", None)
            .await
        {
            Ok(result) => {
                if result.should_silent_drop {
                    println!("  第 {} 次 RPC: Shadow Ban 触发！", i + 1);
                    break;
                }
            }
            Err(_) => {
                println!("  第 {} 次 RPC: 被限流", i + 1);
                break;
            }
        }
    }

    // ============================================
    // 场景 4: 状态查询和管理
    // ============================================
    println!("\n📊 场景 4: 状态查询");

    if let Some(state) = security_service.get_user_state(user_id, device_id).await {
        println!("用户 {} 当前状态: {:?}", user_id, state);
    }

    if let Some(score) = security_service.get_trust_score(user_id, device_id).await {
        println!("用户 {} 信任分: {}", user_id, score);
    }

    // ============================================
    // 场景 5: 手动管理
    // ============================================
    println!("\n👮 场景 5: 管理员操作");

    // 手动封禁
    security_service
        .manual_ban(
            1002,
            "device_2",
            privchat_server::security::ViolationType::Spam,
            Some(Duration::from_secs(3600)),
        )
        .await;
    println!("✅ 已手动封禁用户 1002 (1小时)");

    // 检查被封禁用户的消息发送
    match security_middleware
        .check_send_message(1002, "device_2", 2001, 2, 100, false)
        .await
    {
        Ok(result) => {
            if result.should_silent_drop {
                println!("✅ 用户 1002 处于 Shadow Ban 状态，消息将被静默丢弃");
            }
        }
        Err(e) => println!("❌ 用户 1002 消息被拒绝: {:?}", e),
    }

    // 手动解封
    security_service.manual_unban(1002, "device_2").await;
    println!("✅ 已手动解封用户 1002");

    // ============================================
    // 场景 6: 奖励机制
    // ============================================
    println!("\n🎁 场景 6: 奖励良好行为");

    // 用户正常使用，奖励信任分
    for _ in 0..10 {
        security_middleware
            .reward_good_behavior(user_id, device_id)
            .await;
    }

    if let Some(score) = security_service.get_trust_score(user_id, device_id).await {
        println!(
            "用户 {} 奖励后信任分: {} (信任分会影响限流阈值)",
            user_id, score
        );
    }

    println!("\n✅ ObserveOnly 模式演示完成！");
    println!("   建议：运行至少2周，收集数据后再升级\n");

    Ok(())
}

/// 演示：EnforceLight 模式（成长期）
async fn demo_enforce_light_mode() -> Result<(), Box<dyn std::error::Error>> {
    println!("========================================");
    println!("🚀 阶段 2: EnforceLight 模式");
    println!("========================================\n");

    let security_config = SecurityConfig::production();
    let security_service = Arc::new(SecurityService::new(security_config));
    let security_middleware = Arc::new(SecurityMiddleware::new(security_service.clone()));

    println!("✅ 安全系统已初始化（EnforceLight 模式）");
    println!("   - 基础限流");
    println!("   - 2级状态机（Normal/Throttled）");
    println!("   - 不启用 Shadow Ban");
    println!("   - 适合成长期（1万-5万用户）\n");

    let user_id = 2001;
    let device_id = "device_light";

    // 正常消息
    let result = security_middleware
        .check_send_message(user_id, device_id, 3001, 5, 100, false)
        .await?;
    println!("✅ 正常消息: 允许发送");

    // 模拟高频发送（会触发限流但不会 Shadow Ban）
    println!("\n模拟高频发送（EnforceLight 模式）...");
    for i in 0..100 {
        match security_middleware
            .check_send_message(user_id, device_id, 3001, 5, 100, false)
            .await
        {
            Ok(result) => {
                if result.should_silent_drop {
                    println!("❌ 这不应该发生！EnforceLight 不应该触发 Shadow Ban");
                    break;
                }
            }
            Err(_) => {
                println!("  第 {} 条消息: 被限流（正常拒绝，不是 Shadow Ban）", i + 1);
                break;
            }
        }
    }

    println!("\n✅ EnforceLight 模式演示完成！");
    println!("   建议：运行至少1个月，调优参数后再升级\n");

    Ok(())
}

/// 演示：EnforceFull 模式（稳定期）
async fn demo_enforce_full_mode() -> Result<(), Box<dyn std::error::Error>> {
    println!("========================================");
    println!("💪 阶段 3: EnforceFull 模式");
    println!("========================================\n");

    let security_config = SecurityConfig::strict();
    let security_service = Arc::new(SecurityService::new(security_config));
    let security_middleware = Arc::new(SecurityMiddleware::new(security_service.clone()));

    println!("✅ 安全系统已初始化（EnforceFull 模式）");
    println!("   - 全部限流（多维度）");
    println!("   - 4级状态机");
    println!("   - ✨ Shadow Ban 启用");
    println!("   - ✨ Fan-out 成本计算");
    println!("   - 适合稳定期（> 5万用户）\n");

    let user_id = 3001;
    let device_id = "device_full";

    // 正常小群消息
    let result = security_middleware
        .check_send_message(user_id, device_id, 4001, 5, 100, false)
        .await?;
    println!("✅ 小群消息: 允许发送");

    // 大群消息（fan-out 成本高）
    let result = security_middleware
        .check_send_message(user_id, device_id, 4002, 500, 100, false)
        .await?;
    println!("✅ 大群消息: 允许发送（成本更高）");

    // 模拟恶意高频发送（会触发 Shadow Ban）
    println!("\n模拟恶意高频发送（EnforceFull 模式）...");
    for i in 0..200 {
        match security_middleware
            .check_send_message(user_id, device_id, 4001, 5, 100, false)
            .await
        {
            Ok(result) => {
                if result.should_silent_drop {
                    println!("🚫 第 {} 条消息: Shadow Ban 触发！", i + 1);
                    println!("   （消息会假装成功，但不会真正投递）");
                    break;
                }
            }
            Err(_) => {
                println!("  第 {} 条消息: 被限流", i + 1);
            }
        }
    }

    // 查询用户状态
    if let Some(state) = security_service.get_user_state(user_id, device_id).await {
        println!("\n📊 用户最终状态: {:?}", state);
    }

    if let Some(score) = security_service.get_trust_score(user_id, device_id).await {
        println!("📊 用户信任分: {}", score);
    }

    println!("\n✅ EnforceFull 模式演示完成！");
    println!("   这是最强防护，适合稳定运营阶段\n");

    Ok(())
}
