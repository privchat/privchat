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
use crate::infra::cache::{ChatCacheService, BusinessCacheService, UserStatus, ChannelList, GroupMembers, UnreadCounts, FriendRelations};
use crate::infra::redis::RedisService;
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::time::Duration;
use tokio::sync::RwLock;
use serde_json;

// 暂时禁用 Redis 缓存功能
#[cfg(feature = "redis")]
pub use redis_cache_impl::*;

/// 默认的缓存服务别名
pub type DefaultCacheService = ChatCacheService;

#[cfg(feature = "redis")]
mod redis_cache_impl {
    use super::*;
    
    /// Redis 缓存服务
    pub struct RedisCacheService {
        redis_service: Arc<RedisService>,
        local_cache: Arc<ChatCacheService>,
        config: RedisCacheConfig,
    }
    
    impl RedisCacheService {
        pub fn new(
            redis_service: Arc<RedisService>,
            local_cache: Arc<ChatCacheService>,
            config: RedisCacheConfig,
        ) -> Self {
            Self {
                redis_service,
                local_cache,
                config,
            }
        }
    }
    
    #[derive(Debug, Clone)]
    pub struct RedisCacheConfig {
        pub default_ttl: Duration,
        pub max_retries: u32,
        pub retry_delay: Duration,
    }
    
    impl Default for RedisCacheConfig {
        fn default() -> Self {
            Self {
                default_ttl: Duration::from_secs(3600),
                max_retries: 3,
                retry_delay: Duration::from_millis(100),
            }
        }
    }
} 