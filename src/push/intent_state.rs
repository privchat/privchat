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

    /// 按「消息 + 用户」取消 Intent。
    ///
    /// 长连接投递成功时用它把这条消息的推送拦下来。不能复用 mark_cancelled(user_id)：
    /// 那会把该用户**所有**待发推送都取消掉，包括其它会话里他还没收到的消息。
    /// 也不能复用 mark_cancelled_by_device：用户级 Intent 的 device_id 是空串。
    pub async fn mark_cancelled_by_message_user(&self, message_id: u64, user_id: u64) -> usize {
        let intent_ids = {
            let map = self.intent_by_message.read().await;
            map.get(&message_id).cloned().unwrap_or_default()
        };
        if intent_ids.is_empty() {
            return 0;
        }
        let user_intents = {
            let map = self.intents_by_user.read().await;
            map.get(&user_id).cloned().unwrap_or_default()
        };
        let mut count = 0;
        let mut status = self.intent_status.write().await;
        for intent_id in intent_ids {
            if !user_intents.contains(&intent_id) {
                continue;
            }
            if let Some(st) = status.get_mut(&intent_id) {
                if *st == IntentStatus::Pending {
                    *st = IntentStatus::Cancelled;
                    count += 1;
                }
            }
        }
        count
    }

    /// 这条消息曾经为哪些用户排过推送。
    ///
    /// 撤回时要给这些人补一条静默推送去删通知——**包括状态已经是 Sent 的**：
    /// 恰恰是那些已经发出去的才需要删，没发出去的直接标 revoked 就够了。
    pub async fn users_with_intents_for_message(&self, message_id: u64) -> Vec<u64> {
        let ids = {
            let map = self.intent_by_message.read().await;
            map.get(&message_id).cloned().unwrap_or_default()
        };
        if ids.is_empty() {
            return Vec::new();
        }
        let by_user = self.intents_by_user.read().await;
        let mut users: Vec<u64> = by_user
            .iter()
            .filter(|(_, intents)| intents.iter().any(|i| ids.contains(i)))
            .map(|(user, _)| *user)
            .collect();
        users.sort_unstable();
        users.dedup();
        users
    }

    /// 诊断用：这条消息名下都有哪些 intent、各自什么状态、属于哪个用户。
    pub async fn debug_intents_for_message(&self, message_id: u64) -> Vec<(String, String)> {
        let ids = {
            let map = self.intent_by_message.read().await;
            map.get(&message_id).cloned().unwrap_or_default()
        };
        let status = self.intent_status.read().await;
        let by_user = self.intents_by_user.read().await;
        ids.into_iter()
            .map(|id| {
                let st = status
                    .get(&id)
                    .map(|s| format!("{:?}", s))
                    .unwrap_or_else(|| "missing".to_string());
                let owner = by_user
                    .iter()
                    .find(|(_, v)| v.contains(&id))
                    .map(|(u, _)| u.to_string())
                    .unwrap_or_else(|| "?".to_string());
                (id, format!("{st}/user={owner}"))
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 长连接把消息送到了 → 这条消息对这个用户的推送必须被取消。
    ///
    /// 🔴 只能取消「这一条」，不能牵连该用户其它会话里待发的推送。
    /// mark_cancelled(user_id) 会把用户所有待发推送一起清掉，那会让他错过
    /// 其它会话真正没收到的消息——这正是不复用它的原因。
    #[tokio::test]
    async fn delivery_cancels_only_that_message_for_that_user() {
        let state = IntentStateManager::new();
        state.register_intent("i-a", 100, 52).await;
        state.register_intent("i-b", 200, 52).await; // 同一用户，另一条消息
        state.register_intent("i-c", 100, 99).await; // 同一条消息，另一个用户

        let cancelled = state.mark_cancelled_by_message_user(100, 52).await;
        assert_eq!(cancelled, 1);

        assert_eq!(state.get_status("i-a").await, Some(IntentStatus::Cancelled));
        assert_eq!(
            state.get_status("i-b").await,
            Some(IntentStatus::Pending),
            "同一用户的其它消息不该被牵连"
        );
        assert_eq!(
            state.get_status("i-c").await,
            Some(IntentStatus::Pending),
            "同一条消息发给别人的那份不该被牵连"
        );
    }

    /// 撤回把这条消息的所有 Intent 都标成 revoked（含多设备）。
    #[tokio::test]
    async fn revoke_covers_every_device_of_that_message() {
        let state = IntentStateManager::new();
        state.register_device_intent("d1", 300, 52, "iphone").await;
        state.register_device_intent("d2", 300, 52, "ipad").await;

        assert_eq!(state.mark_revoked(300).await, 2);
        assert_eq!(state.get_status("d1").await, Some(IntentStatus::Revoked));
        assert_eq!(state.get_status("d2").await, Some(IntentStatus::Revoked));
    }
}
