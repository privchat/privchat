/// pts (Persistent Timestamp Sequence) 相关数据结构
///
/// ⚠️ 关键修正（2026-02-11）：
/// - pts 是 **per-channel** 单调递增（NOT per-user）
/// - key 仅使用 channel_id（u64），channel_type 不参与 key
/// - 设备通过比对 local_pts 和 server_pts 实现增量同步
///
/// 为什么是 per-channel？
/// 1. 群聊场景：群聊的消息顺序应该独立于其他群
/// 2. 分片扩展：不同频道可以分布在不同服务器
/// 3. 隔离性：一个频道的消息不影响其他频道的 pts
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

/// pts 生成器（per-channel）⭐
#[derive(Debug, Clone)]
pub struct PtsGenerator {
    /// 每个频道的 pts 计数器
    counters: Arc<RwLock<HashMap<u64, Arc<AtomicU64>>>>,
}

impl PtsGenerator {
    /// 创建新的 pts 生成器
    pub fn new() -> Self {
        Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 生成下一个 pts（原子操作，线程安全）⭐
    ///
    /// # 参数
    /// - channel_id: 频道 ID
    pub async fn next_pts(&self, channel_id: u64) -> u64 {
        let mut counters = self.counters.write().await;
        let counter = counters
            .entry(channel_id)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        counter.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// 获取当前 pts（不递增）
    pub async fn current_pts(&self, channel_id: u64) -> u64 {
        let counters = self.counters.read().await;
        counters
            .get(&channel_id)
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    /// 设置 pts（用于初始化或恢复）
    pub async fn set_pts(&self, channel_id: u64, pts: u64) {
        let mut counters = self.counters.write().await;
        let counter = counters
            .entry(channel_id)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        counter.store(pts, Ordering::SeqCst);
    }

    /// 批量初始化（从数据库恢复）
    pub async fn init_from_db(&self, channel_pts_map: HashMap<u64, u64>) {
        let mut counters = self.counters.write().await;
        for (channel_id, pts) in channel_pts_map {
            counters.insert(channel_id, Arc::new(AtomicU64::new(pts)));
        }
    }
}

impl Default for PtsGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// 带 pts 的消息（用于多设备同步）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageWithPts {
    /// pts 指针（全局唯一递增）
    pub pts: u64,

    /// 消息 ID
    pub message_id: String,

    /// 发送者 ID
    pub from_user_id: String,

    /// 接收者 ID
    pub to_user_id: String,

    /// 频道 ID
    pub channel_id: String,

    /// 消息序号（客户端生成）
    pub local_message_id: String,

    /// 消息类型
    pub message_type: String,

    /// 消息内容
    pub content: String,

    /// 元数据（JSON）
    pub metadata: Option<serde_json::Value>,

    /// 创建时间（Unix 时间戳）
    pub created_at: i64,

    /// 是否已撤回
    pub is_revoked: bool,
}

/// 设备同步状态（每个设备一个）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSyncState {
    /// 用户 ID
    pub user_id: String,

    /// 设备 ID
    pub device_id: String,

    /// 设备本地 pts
    pub local_pts: u64,

    /// 服务端最新 pts
    pub server_pts: u64,

    /// gap（待同步消息数）
    pub gap: u64,

    /// 最后同步时间
    pub last_sync_time: i64,

    /// 最后在线时间
    pub last_online_time: i64,
}

impl DeviceSyncState {
    /// 创建新的设备同步状态
    pub fn new(user_id: String, device_id: String, local_pts: u64) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            user_id,
            device_id,
            local_pts,
            server_pts: 0,
            gap: 0,
            last_sync_time: now,
            last_online_time: now,
        }
    }

    /// 更新同步状态
    pub fn update(&mut self, server_pts: u64) {
        self.server_pts = server_pts;
        self.gap = server_pts.saturating_sub(self.local_pts);
        self.last_sync_time = chrono::Utc::now().timestamp();
    }

    /// 计算 gap
    pub fn calculate_gap(&self) -> u64 {
        self.server_pts.saturating_sub(self.local_pts)
    }
}

/// 用户消息索引（用于快速查找 pts 范围内的消息）
#[derive(Debug, Clone)]
pub struct UserMessageIndex {
    /// 用户 ID -> (pts -> message_id) 映射
    index: Arc<RwLock<HashMap<u64, HashMap<u64, u64>>>>,
}

impl UserMessageIndex {
    /// 创建新的消息索引
    pub fn new() -> Self {
        Self {
            index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 添加消息到索引
    pub async fn add_message(&self, user_id: u64, pts: u64, message_id: u64) {
        let mut index = self.index.write().await;
        let user_index = index.entry(user_id).or_insert_with(HashMap::new);
        user_index.insert(pts, message_id);
    }

    /// 获取 pts 范围内的消息 ID 列表
    pub async fn get_message_ids(&self, user_id: u64, from_pts: u64, to_pts: u64) -> Vec<u64> {
        let index = self.index.read().await;
        if let Some(user_index) = index.get(&user_id) {
            let mut message_ids = Vec::new();
            for pts in from_pts..=to_pts {
                if let Some(message_id) = user_index.get(&pts) {
                    message_ids.push(*message_id);
                }
            }
            message_ids
        } else {
            Vec::new()
        }
    }

    /// 获取 pts > min_pts 的所有消息 ID 列表
    pub async fn get_message_ids_above(&self, user_id: u64, min_pts: u64) -> Vec<u64> {
        let index = self.index.read().await;
        if let Some(user_index) = index.get(&user_id) {
            let mut message_ids = Vec::new();
            for (pts, message_id) in user_index.iter() {
                if *pts > min_pts {
                    message_ids.push(*message_id);
                }
            }
            message_ids
        } else {
            Vec::new()
        }
    }

    /// 获取 pts > min_pts 的所有消息 ID 和 pts 的映射
    pub async fn get_message_ids_with_pts_above(
        &self,
        user_id: u64,
        min_pts: u64,
    ) -> std::collections::HashMap<u64, u64> {
        let index = self.index.read().await;
        if let Some(user_index) = index.get(&user_id) {
            let mut map = std::collections::HashMap::new();
            for (pts, message_id) in user_index.iter() {
                if *pts > min_pts {
                    map.insert(*message_id, *pts);
                }
            }
            map
        } else {
            std::collections::HashMap::new()
        }
    }

    /// 清理旧消息（保留最近 N 条）
    pub async fn cleanup(&self, user_id: u64, keep_latest: usize) {
        let mut index = self.index.write().await;
        if let Some(user_index) = index.get_mut(&user_id) {
            if user_index.len() > keep_latest {
                // 获取所有 pts 并排序
                let mut pts_list: Vec<u64> = user_index.keys().copied().collect();
                pts_list.sort_unstable();

                // 保留最新的 N 条，删除旧的
                let remove_count = pts_list.len() - keep_latest;
                for pts in pts_list.iter().take(remove_count) {
                    user_index.remove(pts);
                }
            }
        }
    }
}

impl Default for UserMessageIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// 离线消息队列配置
#[derive(Debug, Clone)]
pub struct OfflineQueueConfig {
    /// 最大队列长度
    pub max_queue_size: usize,

    /// 批量推送大小
    pub batch_size: usize,

    /// 过期时间（秒）
    pub expire_seconds: i64,
}

impl Default for OfflineQueueConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 5000,          // 最多 5000 条
            batch_size: 50,                // 每批 50 条
            expire_seconds: 7 * 24 * 3600, // 7 天
        }
    }
}

/// 未读计数管理器
#[derive(Debug, Clone)]
pub struct UnreadCounter {
    /// 用户未读计数（user_id -> (channel_id -> count)）
    counters: Arc<RwLock<HashMap<u64, HashMap<u64, u64>>>>,
}

impl UnreadCounter {
    /// 创建新的未读计数管理器
    pub fn new() -> Self {
        Self {
            counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 增加未读计数
    pub async fn increment(&self, user_id: u64, channel_id: u64, count: u64) {
        let mut counters = self.counters.write().await;
        let user_counters = counters.entry(user_id).or_insert_with(HashMap::new);
        *user_counters.entry(channel_id).or_insert(0) += count;
    }

    /// 获取未读计数
    pub async fn get(&self, user_id: u64) -> HashMap<u64, u64> {
        let counters = self.counters.read().await;
        counters.get(&user_id).cloned().unwrap_or_default()
    }

    /// 清空未读计数
    pub async fn clear(&self, user_id: u64) {
        let mut counters = self.counters.write().await;
        counters.remove(&user_id);
    }

    /// 清空特定会话的未读计数
    pub async fn clear_channel(&self, user_id: u64, channel_id: u64) {
        let mut counters = self.counters.write().await;
        if let Some(user_counters) = counters.get_mut(&user_id) {
            user_counters.remove(&channel_id);
        }
    }
}

impl Default for UnreadCounter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pts_generator() {
        let generator = PtsGenerator::new();

        // 测试生成 pts
        let pts1 = generator.next_pts("alice").await;
        assert_eq!(pts1, 1);

        let pts2 = generator.next_pts("alice").await;
        assert_eq!(pts2, 2);

        // 不同用户的 pts 独立
        let pts3 = generator.next_pts("bob").await;
        assert_eq!(pts3, 1);

        // 获取当前 pts
        let current = generator.current_pts("alice").await;
        assert_eq!(current, 2);
    }

    #[tokio::test]
    async fn test_device_sync_state() {
        let mut state = DeviceSyncState::new("alice".to_string(), "device_001".to_string(), 100);

        // 更新同步状态
        state.update(150);
        assert_eq!(state.server_pts, 150);
        assert_eq!(state.gap, 50);
    }

    #[tokio::test]
    async fn test_user_message_index() {
        let index = UserMessageIndex::new();

        // 添加消息
        index.add_message("alice", 1, "msg_001".to_string()).await;
        index.add_message("alice", 2, "msg_002".to_string()).await;
        index.add_message("alice", 3, "msg_003".to_string()).await;

        // 获取范围内的消息
        let message_ids = index.get_message_ids("alice", 1, 2).await;
        assert_eq!(message_ids.len(), 2);
        assert_eq!(message_ids[0], "msg_001");
        assert_eq!(message_ids[1], "msg_002");
    }

    #[tokio::test]
    async fn test_unread_counter() {
        let counter = UnreadCounter::new();

        // 增加未读计数
        counter.increment("alice", "channel_001", 1).await;
        counter.increment("alice", "channel_001", 1).await;
        counter.increment("alice", "channel_002", 3).await;

        // 获取未读计数
        let unread = counter.get("alice").await;
        assert_eq!(unread.get("channel_001"), Some(&2));
        assert_eq!(unread.get("channel_002"), Some(&3));

        // 清空特定会话
        counter.clear_channel("alice", "channel_001").await;
        let unread = counter.get("alice").await;
        assert_eq!(unread.get("channel_001"), None);
        assert_eq!(unread.get("channel_002"), Some(&3));
    }
}
