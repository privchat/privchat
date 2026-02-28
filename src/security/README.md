# 🔐 IM 服务器安全防护系统

> 参考 Telegram / Discord 的工程实践，基于**分层 + 状态机 + 信任度**模型

## ⚠️ 重要提示

**请先阅读：**[SECURITY_ROLLOUT_GUIDE.md](../../../SECURITY_ROLLOUT_GUIDE.md)

**核心原则：架构先到位，能力分阶段启用**

- 🐣 早期（< 1万用户）→ `ObserveOnly` 模式（只记录）
- 🚀 成长（1-5万用户）→ `EnforceLight` 模式（基础限流）
- 💪 稳定（> 5万用户）→ `EnforceFull` 模式（全部特性）

## 🎯 核心特性

✅ **四级状态机** - Normal → Throttled → WriteDisabled → ShadowBanned → Disconnected  
✅ **信任度模型** - 动态调整限流阈值，奖励良好行为  
✅ **多维度限流** - 用户、RPC、会话、群组 四个维度  
✅ **Fan-out 成本计算** - 大群发消息 = N 次写操作  
✅ **Shadow Ban** - 假装成功但不执行（对抗恶意行为的核武器）  
✅ **Sharded Token Bucket** - 高并发无锁设计  
✅ **IP 保护** - 但不过度依赖（现代网络 IP 不可信）  
✅ **分阶段启用** - 避免早期过度处罚

## 📁 模块结构

```
security/
├── mod.rs                  # 模块入口
├── client_state.rs         # 状态机 + 信任度模型
├── rate_limiter.rs         # 多维度限流器
├── security_service.rs     # 中央安全服务
└── README.md              # 本文档
```

## 🚀 快速开始（30秒上手）

### 步骤 1: 配置文件（推荐早期使用）

```toml
# config.toml

[security]
mode = "observe"              # 只记录，不处罚（早期推荐）
enable_shadow_ban = false     # 早期不启用
enable_ip_ban = true

[security.rate_limit]
user_tokens_per_second = 100.0     # 宽松
user_burst_capacity = 200.0
channel_messages_per_second = 10.0
ip_connections_per_second = 10.0
```

### 步骤 2: 在 server.rs 中初始化

```rust
use privchat::security::{SecurityService, SecurityConfig};
use privchat::middleware::SecurityMiddleware;

// 从配置创建安全服务
let security_config = config.security.clone().into();
let security_service = Arc::new(SecurityService::new(security_config));

// 创建安全中间件
let security_middleware = Arc::new(SecurityMiddleware::new(security_service.clone()));

// 启动定期清理任务
let security_service_clone = security_service.clone();
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(3600));
    loop {
        interval.tick().await;
        security_service_clone.cleanup_expired_data().await;
    }
});
```

### 步骤 3: 集成到 handler

代码示例见下方 👇

### 2. 连接建立时检查

```rust
// 在 ConnectionEstablished 事件中
security_middleware.check_connection(&peer_ip).await?;
```

### 3. 消息发送时检查

```rust
// 在 SendMessageHandler 中
let result = security_middleware.check_send_message(
    user_id,
    device_id,
    channel_id,
    recipient_count,  // 群成员数（用于计算 fan-out 成本）
    message_size,
    is_media,
).await?;

if result.should_silent_drop {
    // Shadow Ban：假装成功，但不投递
    return Ok(success_response());
}

// 正常投递消息
deliver_message(...).await?;
```

### 4. RPC 调用时检查

```rust
// 在 RPCMessageHandler 中
let result = security_middleware.check_rpc(
    user_id,
    device_id,
    rpc_method,
    channel_id,
).await?;

if result.should_silent_drop {
    // Shadow Ban：假装成功，但不执行
    return Ok(fake_success_response());
}

// 正常执行 RPC
execute_rpc(...).await?;
```

## 🎚️ 三个安全模式

### ObserveOnly（早期推荐）

```rust
let config = SecurityConfig::early_stage();
```

- ✅ 记录所有行为
- ✅ 打日志和指标
- ❌ 不限流
- ❌ 不处罚
- **适用：** < 1万用户，协议不稳定

### EnforceLight（成长期）

```rust
let config = SecurityConfig::production();
```

- ✅ 基础限流
- ✅ 2级状态机（Normal/Throttled）
- ❌ 不启用 Shadow Ban
- ❌ 不计算 fan-out
- **适用：** 1-5万用户，协议稳定

### EnforceFull（稳定期）

```rust
let config = SecurityConfig::strict();
```

- ✅ 全部限流（多维度）
- ✅ 4级状态机
- ✅ Shadow Ban
- ✅ Fan-out 成本
- **适用：** > 5万用户，成熟产品

## 📊 关键概念

### 状态机

```
┌─────────┐     违规     ┌───────────┐     持续违规     ┌───────────────┐
│ Normal  │ ─────────> │ Throttled │ ────────────> │ WriteDisabled │
└─────────┘            └───────────┘                └───────────────┘
     ↑                       │                              │
     │                       │                              │ 继续违规
     │ 时间恢复               │ 延迟500ms                    ↓
     │                       │                    ┌─────────────────┐
     └───────────────────────┘                    │  ShadowBanned   │
                                                  └─────────────────┘
                                                          │
                                                          │ 协议攻击
                                                          ↓
                                                  ┌─────────────────┐
                                                  │  Disconnected   │
                                                  └─────────────────┘
```

### 信任分影响

| 分数范围 | 限流倍数 | 说明 |
|---------|---------|------|
| 0-100 | 0.5x | 低信任（新用户、违规用户） |
| 101-300 | 0.8x | 中低信任 |
| 301-600 | 1.0x | 正常信任 |
| 601-800 | 1.5x | 高信任 |
| 801-1000 | 2.0x | 超高信任 |

### Fan-out 成本

| 群大小 | 成本倍数 | 说明 |
|--------|---------|------|
| 1-10人 | 1x | 小群，正常 |
| 11-100人 | 2x | 中群 |
| 101-500人 | 5x | 大群 |
| 501-1000人 | 10x | 超大群 |
| 1000+人 | 20x | 频道 |

## 🎓 设计原则

### ❌ 不要做的事

1. **不要用固定数值思维** - 不是"10条/秒"这么简单
2. **不要过度依赖 IP 封禁** - 现代网络 IP 不可信（NAT/VPN）
3. **不要忽略 fan-out 成本** - 大群发消息成本极高
4. **不要立即永久封禁** - 要有渐进式处罚

### ✅ 应该做的事

1. **用状态机管理客户端状态** - 渐进式处罚
2. **用信任度模型动态调整** - 奖励良好行为
3. **考虑 fan-out 成本** - 群大小直接影响成本
4. **使用 Shadow Ban** - 让攻击者以为成功了
5. **多维度限流** - 用户、RPC、会话、群组

## 📈 监控指标

应该监控的关键指标：

```rust
// 1. 限流触发率
let rate_limit_triggers = total_rate_limits / total_requests;

// 2. 状态分布
let state_distribution = {
    normal: count_of_normal,
    throttled: count_of_throttled,
    write_disabled: count_of_write_disabled,
    shadow_banned: count_of_shadow_banned,
    disconnected: count_of_disconnected,
};

// 3. 信任分分布
let trust_score_distribution = histogram_of_trust_scores;

// 4. Fan-out 成本分布
let fanout_cost_distribution = histogram_of_fanout_costs;

// 5. IP 封禁数量
let ip_bans = {
    temp_bans: count_of_temp_bans,
    permanent_bans: count_of_permanent_bans,
};
```

## 🔧 配置调优

### 场景 1: 小型测试服务器

```toml
[security.rate_limit]
user_tokens_per_second = 30.0
user_burst_capacity = 60.0
channel_messages_per_second = 5.0
```

### 场景 2: 生产环境

```toml
[security.rate_limit]
user_tokens_per_second = 50.0
user_burst_capacity = 100.0
channel_messages_per_second = 3.0
```

### 场景 3: 严格安全模式

```toml
[security]
enable_shadow_ban = true
enable_ip_ban = true

[security.rate_limit]
user_tokens_per_second = 20.0
channel_messages_per_second = 2.0
ip_connections_per_second = 3.0
```

## 🧪 测试

运行示例：

```bash
cargo run --example security_integration
```

运行测试：

```bash
cargo test --package privchat-server --lib security
```

## 📚 相关文档

- [完整设计文档](../../../SECURITY_DESIGN.md)
- [配置示例](../../config.example.toml)
- [集成示例](../../examples/security_integration.rs)

## 🤝 贡献

如有改进建议，欢迎提交 PR。

## 📝 许可证

与主项目相同。
