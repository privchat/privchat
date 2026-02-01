use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::error::Result;
use crate::infra::CacheManager;

/// 黑名单条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlacklistEntry {
    /// 拉黑者 ID
    pub user_id: u64,
    /// 被拉黑用户 ID
    pub blocked_user_id: u64,
    /// 拉黑时间
    pub blocked_at: DateTime<Utc>,
    /// 拉黑原因（可选）
    pub reason: Option<String>,
}

/// 黑名单服务
pub struct BlacklistService {
    /// 黑名单存储：user_id -> HashSet<blocked_user_id>
    /// 内存存储，key: user_id, value: Vec<BlacklistEntry>
    blacklist_store: Arc<RwLock<HashMap<u64, Vec<BlacklistEntry>>>>,
    cache_manager: Arc<CacheManager>,
}

impl BlacklistService {
    /// 创建新的黑名单服务
    pub fn new(cache_manager: Arc<CacheManager>) -> Self {
        Self {
            blacklist_store: Arc::new(RwLock::new(HashMap::new())),
            cache_manager,
        }
    }

    /// 添加用户到黑名单
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `blocked_user_id` - 被拉黑用户 ID
    /// * `reason` - 拉黑原因（可选）
    ///
    /// # Returns
    /// 黑名单条目
    pub async fn add_to_blacklist(
        &self,
        user_id: u64,
        blocked_user_id: u64,
        reason: Option<String>,
    ) -> Result<BlacklistEntry> {
        // 检查是否是自己
        if user_id == blocked_user_id {
            return Err(crate::error::ServerError::Validation(
                "不能拉黑自己".to_string(),
            ));
        }

        let mut store = self.blacklist_store.write().await;
        let user_blacklist = store.entry(user_id).or_insert_with(Vec::new);

        // 检查是否已经在黑名单中
        if user_blacklist.iter().any(|entry| entry.blocked_user_id == blocked_user_id) {
            info!("用户 {} 已在 {} 的黑名单中", blocked_user_id, user_id);
            // 返回现有条目
            return Ok(user_blacklist
                .iter()
                .find(|entry| entry.blocked_user_id == blocked_user_id)
                .cloned()
                .unwrap());
        }

        // 创建新的黑名单条目
        let entry = BlacklistEntry {
            user_id,
            blocked_user_id,
            blocked_at: Utc::now(),
            reason,
        };

        user_blacklist.push(entry.clone());
        info!(
            "✅ 用户 {} 已将 {} 加入黑名单 (原因: {:?})",
            user_id, blocked_user_id, entry.reason
        );

        Ok(entry)
    }

    /// 从黑名单移除用户
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `blocked_user_id` - 被拉黑用户 ID
    ///
    /// # Returns
    /// 是否成功移除
    pub async fn remove_from_blacklist(
        &self,
        user_id: u64,
        blocked_user_id: u64,
    ) -> Result<bool> {
        let mut store = self.blacklist_store.write().await;

        if let Some(user_blacklist) = store.get_mut(&user_id) {
            let original_len = user_blacklist.len();
            user_blacklist.retain(|entry| entry.blocked_user_id != blocked_user_id);

            if user_blacklist.len() < original_len {
                info!("✅ 用户 {} 已将 {} 从黑名单移除", user_id, blocked_user_id);
                return Ok(true);
            }
        }

        warn!("⚠️ 用户 {} 的黑名单中未找到 {}", user_id, blocked_user_id);
        Ok(false)
    }

    /// 检查用户是否在黑名单中
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `target_user_id` - 要检查的用户 ID
    ///
    /// # Returns
    /// 是否在黑名单中
    pub async fn is_blocked(&self, user_id: u64, target_user_id: u64) -> Result<bool> {
        let store = self.blacklist_store.read().await;

        if let Some(user_blacklist) = store.get(&user_id) {
            let is_blocked = user_blacklist
                .iter()
                .any(|entry| entry.blocked_user_id == target_user_id);
            debug!(
                "检查黑名单: {} 是否拉黑 {} = {}",
                user_id, target_user_id, is_blocked
            );
            return Ok(is_blocked);
        }

        debug!("用户 {} 没有黑名单记录", user_id);
        Ok(false)
    }

    /// 获取用户的黑名单列表
    ///
    /// # Arguments
    /// * `user_id` - 用户 ID
    ///
    /// # Returns
    /// 黑名单条目列表
    pub async fn get_blacklist(&self, user_id: u64) -> Result<Vec<BlacklistEntry>> {
        let store = self.blacklist_store.read().await;

        if let Some(user_blacklist) = store.get(&user_id) {
            debug!("获取用户 {} 的黑名单，共 {} 个", user_id, user_blacklist.len());
            return Ok(user_blacklist.clone());
        }

        debug!("用户 {} 没有黑名单记录", user_id);
        Ok(Vec::new())
    }

    /// 获取黑名单条目（如果存在）
    ///
    /// # Arguments
    /// * `user_id` - 拉黑者 ID
    /// * `blocked_user_id` - 被拉黑用户 ID
    ///
    /// # Returns
    /// 黑名单条目（如果存在）
    pub async fn get_blacklist_entry(
        &self,
        user_id: u64,
        blocked_user_id: u64,
    ) -> Result<Option<BlacklistEntry>> {
        let store = self.blacklist_store.read().await;

        if let Some(user_blacklist) = store.get(&user_id) {
            if let Some(entry) = user_blacklist
                .iter()
                .find(|entry| entry.blocked_user_id == blocked_user_id)
            {
                return Ok(Some(entry.clone()));
            }
        }

        Ok(None)
    }

    /// 检查两个用户之间是否存在任意方向的拉黑关系
    ///
    /// # Arguments
    /// * `user_a` - 用户A ID
    /// * `user_b` - 用户B ID
    ///
    /// # Returns
    /// (A是否拉黑B, B是否拉黑A)
    pub async fn check_mutual_block(
        &self,
        user_a: u64,
        user_b: u64,
    ) -> Result<(bool, bool)> {
        let a_blocks_b = self.is_blocked(user_a, user_b).await?;
        let b_blocks_a = self.is_blocked(user_b, user_a).await?;
        Ok((a_blocks_b, b_blocks_a))
    }

    /// 获取黑名单统计信息
    ///
    /// # Arguments
    /// * `user_id` - 用户 ID
    ///
    /// # Returns
    /// 黑名单数量
    pub async fn get_blacklist_count(&self, user_id: u64) -> Result<usize> {
        let store = self.blacklist_store.read().await;
        Ok(store.get(&user_id).map(|list| list.len()).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_blacklist_basic_operations() {
        let cache_manager = Arc::new(CacheManager::new_simple());
        let service = BlacklistService::new(cache_manager);

        // 添加黑名单
        let entry = service
            .add_to_blacklist(1, 2, Some("骚扰".to_string()))
            .await
            .unwrap();
        assert_eq!(entry.blocked_user_id, 2);

        // 检查是否在黑名单
        assert!(service.is_blocked(1, 2).await.unwrap());
        assert!(!service.is_blocked(2, 1).await.unwrap());

        // 获取黑名单列表
        let blacklist = service.get_blacklist(1).await.unwrap();
        assert_eq!(blacklist.len(), 1);

        // 移除黑名单
        assert!(service.remove_from_blacklist(1, 2).await.unwrap());
        assert!(!service.is_blocked(1, 2).await.unwrap());
    }

    #[tokio::test]
    async fn test_cannot_block_self() {
        let cache_manager = Arc::new(CacheManager::new_simple());
        let service = BlacklistService::new(cache_manager);

        let result = service
            .add_to_blacklist(1, 1, None)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mutual_block_check() {
        let cache_manager = Arc::new(CacheManager::new_simple());
        let service = BlacklistService::new(cache_manager);

        service.add_to_blacklist(1, 2, None).await.unwrap();
        service.add_to_blacklist(2, 1, None).await.unwrap();

        let (a_blocks_b, b_blocks_a) = service
            .check_mutual_block(1, 2)
            .await
            .unwrap();
        assert!(a_blocks_b);
        assert!(b_blocks_a);
    }
}

