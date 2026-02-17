use crate::push::types::IntentStatus;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Intent 状态管理器（共享状态）
///
/// 用于 Planner 和 Worker 之间共享 Intent 状态
/// Phase 3: 支持撤销和取消推送
pub struct IntentStateManager {
    // message_id -> intent_ids 映射（用于撤销，Phase 3.5: 一个消息可能有多个设备级 Intent）
    intent_by_message: Arc<RwLock<HashMap<u64, Vec<String>>>>,
    // user_id -> intent_ids 映射（用于取消）
    intents_by_user: Arc<RwLock<HashMap<u64, Vec<String>>>>,
    // ✨ Phase 3.5: device_id -> intent_ids 映射（用于设备级取消）
    intents_by_device: Arc<RwLock<HashMap<String, Vec<String>>>>,
    // intent_id -> status 映射
    intent_status: Arc<RwLock<HashMap<String, IntentStatus>>>,
}

impl IntentStateManager {
    pub fn new() -> Self {
        Self {
            intent_by_message: Arc::new(RwLock::new(HashMap::new())),
            intents_by_user: Arc::new(RwLock::new(HashMap::new())),
            intents_by_device: Arc::new(RwLock::new(HashMap::new())), // ✨ Phase 3.5
            intent_status: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册 Intent（用户级，兼容旧逻辑）
    pub async fn register_intent(&self, intent_id: &str, message_id: u64, user_id: u64) {
        self.register_device_intent(intent_id, message_id, user_id, "")
            .await;
    }

    /// ✨ Phase 3.5: 注册设备级 Intent
    pub async fn register_device_intent(
        &self,
        intent_id: &str,
        message_id: u64,
        user_id: u64,
        device_id: &str,
    ) {
        // message_id -> intent_ids（一个消息可能有多个设备级 Intent）
        {
            let mut map = self.intent_by_message.write().await;
            map.entry(message_id)
                .or_insert_with(Vec::new)
                .push(intent_id.to_string());
        }

        // user_id -> intent_ids
        {
            let mut map = self.intents_by_user.write().await;
            map.entry(user_id)
                .or_insert_with(Vec::new)
                .push(intent_id.to_string());
        }

        // ✨ Phase 3.5: device_id -> intent_ids
        if !device_id.is_empty() {
            let mut map = self.intents_by_device.write().await;
            map.entry(device_id.to_string())
                .or_insert_with(Vec::new)
                .push(intent_id.to_string());
        }

        // intent_id -> status
        {
            let mut map = self.intent_status.write().await;
            map.insert(intent_id.to_string(), IntentStatus::Pending);
        }
    }

    /// 获取 Intent 状态
    pub async fn get_status(&self, intent_id: &str) -> Option<IntentStatus> {
        let map = self.intent_status.read().await;
        map.get(intent_id).copied()
    }

    /// 标记 Intent 为 revoked（Phase 3.5: 支持多个设备级 Intent）
    pub async fn mark_revoked(&self, message_id: u64) -> usize {
        // 查找所有 intent_ids
        let intent_ids = {
            let map = self.intent_by_message.read().await;
            map.get(&message_id).cloned().unwrap_or_default()
        };

        if intent_ids.is_empty() {
            return 0;
        }

        // 标记所有 Intent 为 revoked
        let mut count = 0;
        let mut map = self.intent_status.write().await;
        for intent_id in intent_ids {
            if let Some(status) = map.get_mut(&intent_id) {
                if *status == IntentStatus::Pending {
                    *status = IntentStatus::Revoked;
                    count += 1;
                }
            }
        }

        count
    }

    /// 标记用户的所有待推送 Intent 为 cancelled
    pub async fn mark_cancelled(&self, user_id: u64) -> usize {
        // 查找该用户的所有 Intent
        let intent_ids = {
            let map = self.intents_by_user.read().await;
            map.get(&user_id).cloned().unwrap_or_default()
        };

        if intent_ids.is_empty() {
            return 0;
        }

        // 标记为 cancelled
        let mut count = 0;
        let mut map = self.intent_status.write().await;
        for intent_id in intent_ids {
            if let Some(status) = map.get_mut(&intent_id) {
                if *status == IntentStatus::Pending {
                    *status = IntentStatus::Cancelled;
                    count += 1;
                }
            }
        }

        count
    }

    /// ✨ Phase 3.5: 按设备取消 Intent
    pub async fn mark_cancelled_by_device(
        &self,
        device_id: &str,
        _message_id: Option<u64>,
    ) -> usize {
        // 查找该设备的所有 Intent
        let intent_ids = {
            let map = self.intents_by_device.read().await;
            map.get(device_id).cloned().unwrap_or_default()
        };

        if intent_ids.is_empty() {
            return 0;
        }

        // 如果指定了 message_id，需要过滤（暂时先取消所有，可以优化）
        let mut count = 0;
        let mut map = self.intent_status.write().await;

        for intent_id in intent_ids {
            // TODO: 如果指定了 message_id，需要检查 Intent 的 message_id
            // 暂时先取消所有该设备的 Intent
            if let Some(status) = map.get_mut(&intent_id) {
                if *status == IntentStatus::Pending {
                    *status = IntentStatus::Cancelled;
                    count += 1;
                }
            }
        }

        count
    }

    /// 清理已完成的 Intent（可选，防止内存泄漏）
    pub async fn cleanup_completed(&self, _intent_id: &str) {
        // 从所有映射中移除
        // 注意：这里需要知道 message_id 和 user_id，暂时不实现
        // 未来可以添加一个反向索引
    }
}

impl Default for IntentStateManager {
    fn default() -> Self {
        Self::new()
    }
}
