use crate::auth::models::ServiceKeyConfig;
use crate::error::{Result, ServerError};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Service Key 管理策略
pub enum ServiceKeyStrategy {
    /// 单一主密钥（最简单，类似 Redis 单密码）
    MasterKey(String),

    /// 多密钥白名单
    Whitelist(Arc<RwLock<HashSet<String>>>),

    /// 完全开放（任何密钥都接受，适合内网测试环境）
    AllowAny,
}

/// Service Key 管理器
pub struct ServiceKeyManager {
    strategy: ServiceKeyStrategy,
}

impl ServiceKeyManager {
    /// 创建单一主密钥模式
    pub fn new_master_key(master_key: String) -> Self {
        Self {
            strategy: ServiceKeyStrategy::MasterKey(master_key),
        }
    }

    /// 创建白名单模式
    pub fn new_whitelist(keys: Vec<ServiceKeyConfig>) -> Self {
        let key_set: HashSet<String> = keys.into_iter().map(|k| k.key).collect();
        Self {
            strategy: ServiceKeyStrategy::Whitelist(Arc::new(RwLock::new(key_set))),
        }
    }

    /// 创建完全开放模式
    pub fn new_allow_any() -> Self {
        Self {
            strategy: ServiceKeyStrategy::AllowAny,
        }
    }

    /// 验证 service key
    pub async fn verify(&self, key: &str) -> bool {
        match &self.strategy {
            ServiceKeyStrategy::MasterKey(master) => {
                // 使用恒定时间比较防止时序攻击
                constant_time_compare(key.as_bytes(), master.as_bytes())
            }
            ServiceKeyStrategy::AllowAny => true,
            ServiceKeyStrategy::Whitelist(whitelist) => {
                let keys = whitelist.read().await;
                keys.iter()
                    .any(|k| constant_time_compare(key.as_bytes(), k.as_bytes()))
            }
        }
    }

    /// 添加新的 service key（仅白名单模式支持）
    pub async fn add_key(&self, key: String) -> Result<()> {
        match &self.strategy {
            ServiceKeyStrategy::Whitelist(whitelist) => {
                let mut keys = whitelist.write().await;
                keys.insert(key);
                Ok(())
            }
            _ => Err(ServerError::Internal(
                "当前策略不支持动态添加 key".to_string(),
            )),
        }
    }

    /// 撤销 service key（仅白名单模式支持）
    pub async fn revoke_key(&self, key: &str) -> Result<bool> {
        match &self.strategy {
            ServiceKeyStrategy::Whitelist(whitelist) => {
                let mut keys = whitelist.write().await;
                Ok(keys.remove(key))
            }
            _ => Err(ServerError::Internal("当前策略不支持撤销 key".to_string())),
        }
    }

    /// 列出所有 key（仅白名单模式支持）
    pub async fn list_keys(&self) -> Result<Vec<String>> {
        match &self.strategy {
            ServiceKeyStrategy::Whitelist(whitelist) => {
                let keys = whitelist.read().await;
                Ok(keys.iter().cloned().collect())
            }
            ServiceKeyStrategy::MasterKey(_) => Ok(vec!["[master-key]".to_string()]),
            ServiceKeyStrategy::AllowAny => Ok(vec!["[allow-any]".to_string()]),
        }
    }
}

/// 恒定时间字符串比较，防止时序攻击
fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }

    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_master_key_strategy() {
        let manager = ServiceKeyManager::new_master_key("my-secret-key".to_string());

        assert!(manager.verify("my-secret-key").await);
        assert!(!manager.verify("wrong-key").await);
    }

    #[tokio::test]
    async fn test_whitelist_strategy() {
        let keys = vec![
            ServiceKeyConfig {
                key: "key1".to_string(),
                name: "System 1".to_string(),
            },
            ServiceKeyConfig {
                key: "key2".to_string(),
                name: "System 2".to_string(),
            },
        ];

        let manager = ServiceKeyManager::new_whitelist(keys);

        assert!(manager.verify("key1").await);
        assert!(manager.verify("key2").await);
        assert!(!manager.verify("key3").await);

        // 测试添加
        manager.add_key("key3".to_string()).await.unwrap();
        assert!(manager.verify("key3").await);

        // 测试撤销
        manager.revoke_key("key2").await.unwrap();
        assert!(!manager.verify("key2").await);
    }

    #[tokio::test]
    async fn test_allow_any_strategy() {
        let manager = ServiceKeyManager::new_allow_any();

        assert!(manager.verify("any-key").await);
        assert!(manager.verify("another-key").await);
    }

    #[test]
    fn test_constant_time_compare() {
        assert!(constant_time_compare(b"hello", b"hello"));
        assert!(!constant_time_compare(b"hello", b"world"));
        assert!(!constant_time_compare(b"short", b"verylongstring"));
    }
}
