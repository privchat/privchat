/// 多维度速率限制器
/// 
/// 核心特性：
/// 1. 基于 cost 而不是简单计数
/// 2. 支持 (user, msg_type), (user, channel), (channel) 三层限流
/// 3. 考虑 fan-out 成本
/// 4. 使用 sharded token bucket 保证高并发性能

use std::collections::HashMap;
use std::time::Instant;
use parking_lot::Mutex;

use super::client_state::{TrustScore, ViolationType};

/// RPC 操作成本定义
#[derive(Debug, Clone, Copy)]
pub struct RpcCost {
    /// 令牌消耗
    pub tokens: u32,
    /// 是否是强状态操作
    pub is_stateful: bool,
    /// 是否是敏感操作
    pub is_sensitive: bool,
}

impl RpcCost {
    pub const fn light(tokens: u32) -> Self {
        Self { tokens, is_stateful: false, is_sensitive: false }
    }
    
    pub const fn stateful(tokens: u32) -> Self {
        Self { tokens, is_stateful: true, is_sensitive: false }
    }
    
    pub const fn sensitive(tokens: u32) -> Self {
        Self { tokens, is_stateful: true, is_sensitive: true }
    }
}

/// RPC 成本表（根据你的反馈定义）
pub struct RpcCostTable;

impl RpcCostTable {
    /// 获取 RPC 的成本
    pub fn get_cost(rpc_method: &str) -> RpcCost {
        match rpc_method {
            // === 强状态 RPC（严格控制）===
            "auth.login" | "auth.register" => RpcCost::sensitive(50),
            "sync.pull" | "sync.getDifference" => RpcCost::stateful(20),
            "messages.readHistory" | "messages.ack" => RpcCost::stateful(2),
            "account.updateStatus" => RpcCost::stateful(10),
            
            // === 弱状态 RPC（可放宽）===
            "users.getProfile" | "users.getInfo" => RpcCost::light(1),
            "contacts.getList" => RpcCost::light(5),
            "contacts.search" => RpcCost::light(10),
            
            // === 写操作 ===
            "messages.send" => RpcCost::stateful(5),
            "channels.createChannel" => RpcCost::stateful(20),
            "channels.inviteMembers" => RpcCost::stateful(15),
            
            // === 禁止高频的重型 RPC ===
            "sync.getFullState" => RpcCost::stateful(100),
            "channels.getAllMembers" => RpcCost::stateful(50),
            
            // 默认
            _ => RpcCost::light(5),
        }
    }
}

/// 令牌桶（单个用户/维度）
#[derive(Debug)]
struct TokenBucket {
    /// 当前令牌数
    tokens: f64,
    /// 桶容量
    capacity: f64,
    /// 每秒补充速率
    refill_rate: f64,
    /// 最后更新时间
    last_update: Instant,
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_rate,
            last_update: Instant::now(),
        }
    }
    
    /// 尝试消耗令牌
    fn try_consume(&mut self, cost: f64) -> bool {
        self.refill();
        
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
    
    /// 补充令牌
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_update = now;
    }
    
    /// 获取剩余令牌
    fn remaining(&mut self) -> f64 {
        self.refill();
        self.tokens
    }
}

/// 多维度限流键
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum RateLimitKey {
    /// 用户全局
    User(u64),
    /// 用户 + RPC 方法
    UserRpc(u64, String),
    /// 用户 + 会话
    UserChannel(u64, u64),
    /// 会话（限制单个群的总流量）
    Channel(u64),
    /// IP（仅用于连接层）
    Ip(String),
}

/// 限流配置
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// 用户全局：每秒令牌数
    pub user_tokens_per_second: f64,
    /// 用户全局：桶容量（允许突发）
    pub user_burst_capacity: f64,
    
    /// 单会话：每秒消息数
    pub channel_messages_per_second: f64,
    pub channel_burst_capacity: f64,
    
    /// IP 连接：每秒连接数
    pub ip_connections_per_second: f64,
    pub ip_burst_capacity: f64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            // 用户全局：基础 50 tokens/s，突发 100
            user_tokens_per_second: 50.0,
            user_burst_capacity: 100.0,
            
            // 单会话：3条消息/秒（考虑到大群的 fan-out）
            channel_messages_per_second: 3.0,
            channel_burst_capacity: 10.0,
            
            // IP 连接：5个/秒
            ip_connections_per_second: 5.0,
            ip_burst_capacity: 10.0,
        }
    }
}

/// 分片令牌桶（减少锁竞争）
const SHARD_COUNT: usize = 16;

struct ShardedBuckets {
    shards: [Mutex<HashMap<RateLimitKey, TokenBucket>>; SHARD_COUNT],
}

impl ShardedBuckets {
    fn new() -> Self {
        Self {
            shards: [
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
                Mutex::new(HashMap::new()), Mutex::new(HashMap::new()),
            ],
        }
    }
    
    fn get_shard(&self, key: &RateLimitKey) -> &Mutex<HashMap<RateLimitKey, TokenBucket>> {
        let hash = self.hash_key(key);
        &self.shards[hash % SHARD_COUNT]
    }
    
    fn hash_key(&self, key: &RateLimitKey) -> usize {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish() as usize
    }
}

/// 多维度限流器
pub struct MultiDimensionRateLimiter {
    config: RateLimitConfig,
    buckets: ShardedBuckets,
}

impl MultiDimensionRateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: ShardedBuckets::new(),
        }
    }
    
    /// 检查并消耗限流配额
    /// 
    /// 返回：(是否允许, 违规类型)
    pub fn check_and_consume(
        &self,
        user_id: u64,
        rpc_method: &str,
        channel_id: Option<u64>,
        trust_score: &TrustScore,
    ) -> (bool, Option<ViolationType>) {
        let cost = RpcCostTable::get_cost(rpc_method);
        let multiplier = trust_score.get_rate_multiplier();
        
        // 1. 用户全局限流
        let user_key = RateLimitKey::User(user_id);
        if !self.try_consume_internal(
            &user_key,
            cost.tokens as f64,
            self.config.user_tokens_per_second * multiplier,
            self.config.user_burst_capacity * multiplier,
        ) {
            return (false, Some(ViolationType::RateLimit));
        }
        
        // 2. 用户 + RPC 方法限流
        let user_rpc_key = RateLimitKey::UserRpc(user_id, rpc_method.to_string());
        let rpc_specific_rate = self.get_rpc_specific_rate(rpc_method, multiplier);
        if !self.try_consume_internal(
            &user_rpc_key,
            cost.tokens as f64,
            rpc_specific_rate.0,
            rpc_specific_rate.1,
        ) {
            return (false, Some(ViolationType::RateLimit));
        }
        
        // 3. 会话级限流（如果是发消息操作）
        if let Some(conv_id) = channel_id {
            if rpc_method.starts_with("messages.send") {
                // 用户 + 会话
                let user_conv_key = RateLimitKey::UserChannel(user_id, conv_id);
                if !self.try_consume_internal(
                    &user_conv_key,
                    1.0, // 消息按条计数
                    self.config.channel_messages_per_second * multiplier,
                    self.config.channel_burst_capacity * multiplier,
                ) {
                    return (false, Some(ViolationType::FanoutAbuse));
                }
                
                // 会话全局（防止单个群被刷爆）
                let conv_key = RateLimitKey::Channel(conv_id);
                if !self.try_consume_internal(
                    &conv_key,
                    1.0,
                    self.config.channel_messages_per_second * 10.0, // 群的总限流是单用户的10倍
                    self.config.channel_burst_capacity * 10.0,
                ) {
                    return (false, Some(ViolationType::FanoutAbuse));
                }
            }
        }
        
        (true, None)
    }
    
    /// 内部消耗方法
    fn try_consume_internal(
        &self,
        key: &RateLimitKey,
        cost: f64,
        rate: f64,
        capacity: f64,
    ) -> bool {
        let shard = self.buckets.get_shard(key);
        let mut buckets = shard.lock();
        
        let bucket = buckets
            .entry(key.clone())
            .or_insert_with(|| TokenBucket::new(capacity, rate));
        
        bucket.try_consume(cost)
    }
    
    /// 获取 RPC 特定的速率配置
    fn get_rpc_specific_rate(&self, rpc_method: &str, multiplier: f64) -> (f64, f64) {
        match rpc_method {
            // 认证：严格限制
            "auth.login" | "auth.register" => (1.0 * multiplier, 3.0 * multiplier),
            
            // 同步：中等限制
            "sync.pull" => (2.0 * multiplier, 5.0 * multiplier),
            
            // 搜索：中等限制
            "contacts.search" => (5.0 * multiplier, 10.0 * multiplier),
            
            // 轻量读操作：宽松
            "users.getProfile" => (50.0 * multiplier, 100.0 * multiplier),
            
            // 默认
            _ => (10.0 * multiplier, 20.0 * multiplier),
        }
    }
    
    /// IP 连接限流（连接层使用）
    pub fn check_ip_connection(&self, ip: &str) -> bool {
        let key = RateLimitKey::Ip(ip.to_string());
        self.try_consume_internal(
            &key,
            1.0,
            self.config.ip_connections_per_second,
            self.config.ip_burst_capacity,
        )
    }
}

/// 消息 Fan-out 成本计算器
pub struct FanoutCostCalculator;

impl FanoutCostCalculator {
    /// 计算消息的实际成本
    /// 
    /// 考虑因素：
    /// - 接收者数量（fan-out）
    /// - 消息大小
    /// - 消息类型（文本 < 图片 < 视频）
    pub fn calculate_message_cost(
        recipient_count: usize,
        message_size_bytes: usize,
        is_media: bool,
    ) -> u32 {
        let base_cost = 1u32;
        
        // Fan-out 成本（核心！）
        let fanout_multiplier = match recipient_count {
            1..=10 => 1.0,           // 小群：正常
            11..=100 => 2.0,         // 中群：翻倍
            101..=500 => 5.0,        // 大群：5倍
            501..=1000 => 10.0,      // 超大群：10倍
            _ => 20.0,               // 频道：20倍
        };
        
        // 消息大小成本
        let size_multiplier = match message_size_bytes {
            0..=1024 => 1.0,         // < 1KB
            1025..=10240 => 1.5,     // 1-10KB
            10241..=102400 => 2.0,   // 10-100KB
            _ => 3.0,                // > 100KB
        };
        
        // 媒体成本
        let media_multiplier = if is_media { 2.0 } else { 1.0 };
        
        let total_cost = (base_cost as f64) * fanout_multiplier * size_multiplier * media_multiplier;
        total_cost.ceil() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::client_state::TrustScore;
    
    #[test]
    fn test_rate_limiter() {
        let config = RateLimitConfig::default();
        let limiter = MultiDimensionRateLimiter::new(config);
        let trust = TrustScore::new(1001, "device_1".to_string());
        
        // 正常请求
        let (allowed, _) = limiter.check_and_consume(1001, "users.getProfile", None, &trust);
        assert!(allowed);
        
        // 高频请求
        for _ in 0..200 {
            let (allowed, violation) = limiter.check_and_consume(1001, "users.getProfile", None, &trust);
            if !allowed {
                assert_eq!(violation, Some(ViolationType::RateLimit));
                break;
            }
        }
    }
    
    #[test]
    fn test_fanout_cost() {
        // 小群文本
        let cost1 = FanoutCostCalculator::calculate_message_cost(5, 100, false);
        assert_eq!(cost1, 1);
        
        // 大群文本
        let cost2 = FanoutCostCalculator::calculate_message_cost(200, 100, false);
        assert!(cost2 > 5);
        
        // 大群媒体
        let cost3 = FanoutCostCalculator::calculate_message_cost(200, 50000, true);
        assert!(cost3 > cost2);
    }
}
