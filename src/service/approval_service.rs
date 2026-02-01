use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{Utc, DateTime, Duration};
use tracing::info;
use uuid::Uuid;
use serde::{Serialize, Deserialize};

use crate::error::{ServerError, Result};
use crate::model::channel::{ChannelId, UserId};

/// 加群请求状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinRequestStatus {
    Pending = 0,  // 待审批
    Approved = 1, // 已同意
    Rejected = 2, // 已拒绝
    Expired = 3,  // 已过期
}

/// 加群方式
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinMethod {
    /// 成员邀请
    MemberInvite { inviter_id: String },
    /// 二维码
    QRCode { qr_code_id: String },
}

/// 加群请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub request_id: String,
    pub group_id: ChannelId,
    pub user_id: UserId,
    pub method: JoinMethod,
    pub status: JoinRequestStatus,
    pub message: Option<String>,         // 申请消息
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub handler_id: Option<UserId>,      // 处理人ID
    pub reject_reason: Option<String>,   // 拒绝原因
}

/// 加群审批服务
pub struct ApprovalService {
    /// 存储所有加群请求，key: request_id
    join_requests: Arc<RwLock<HashMap<String, JoinRequest>>>,
    /// 索引：按群组ID索引请求，key: group_id, value: Vec<request_id>
    group_requests_index: Arc<RwLock<HashMap<ChannelId, Vec<String>>>>,
    /// 索引：按用户ID索引请求，key: user_id, value: Vec<request_id>
    user_requests_index: Arc<RwLock<HashMap<UserId, Vec<String>>>>,
}

impl ApprovalService {
    pub fn new() -> Self {
        Self {
            join_requests: Arc::new(RwLock::new(HashMap::new())),
            group_requests_index: Arc::new(RwLock::new(HashMap::new())),
            user_requests_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 创建加群请求
    pub async fn create_join_request(
        &self,
        group_id: ChannelId,
        user_id: UserId,
        method: JoinMethod,
        message: Option<String>,
        expire_hours: Option<i64>,
    ) -> Result<JoinRequest> {
        let request_id = Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let expires_at = expire_hours.map(|hours| created_at + Duration::hours(hours));

        let request = JoinRequest {
            request_id: request_id.clone(),
            group_id: group_id.clone(),
            user_id: user_id.clone(),
            method,
            status: JoinRequestStatus::Pending,
            message,
            created_at,
            updated_at: created_at,
            expires_at,
            handler_id: None,
            reject_reason: None,
        };

        // 存储请求
        self.join_requests.write().await.insert(request_id.clone(), request.clone());

        // 更新索引
        self.group_requests_index.write().await
            .entry(group_id.clone())
            .or_default()
            .push(request_id.clone());

        self.user_requests_index.write().await
            .entry(user_id.clone())
            .or_default()
            .push(request_id.clone());

        info!("✅ 创建加群请求: request_id={}, group_id={}, user_id={}", 
            request_id, group_id, user_id);

        Ok(request)
    }

    /// 获取单个加群请求
    pub async fn get_request(&self, request_id: &str) -> Result<Option<JoinRequest>> {
        Ok(self.join_requests.read().await.get(request_id).cloned())
    }

    /// 获取群组的所有待审批请求
    pub async fn get_pending_requests_by_group(
        &self,
        group_id: u64,
    ) -> Result<Vec<JoinRequest>> {
        let request_ids = self.group_requests_index.read().await
            .get(&group_id)
            .cloned()
            .unwrap_or_default();

        let requests_guard = self.join_requests.read().await;
        let mut requests = Vec::new();

        for request_id in request_ids {
            if let Some(request) = requests_guard.get(&request_id) {
                // 只返回待审批的请求
                if request.status == JoinRequestStatus::Pending {
                    // 检查是否过期
                    if let Some(expires_at) = request.expires_at {
                        if Utc::now() > expires_at {
                            // 已过期，跳过（应该由定期任务清理）
                            continue;
                        }
                    }
                    requests.push(request.clone());
                }
            }
        }

        // 按创建时间倒序排序
        requests.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        Ok(requests)
    }

    /// 获取用户的加群请求历史
    pub async fn get_requests_by_user(
        &self,
        user_id: u64,
    ) -> Result<Vec<JoinRequest>> {
        let request_ids = self.user_requests_index.read().await
            .get(&user_id)
            .cloned()
            .unwrap_or_default();

        let requests_guard = self.join_requests.read().await;
        let mut requests = Vec::new();

        for request_id in request_ids {
            if let Some(request) = requests_guard.get(&request_id) {
                requests.push(request.clone());
            }
        }

        // 按创建时间倒序排序
        requests.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        Ok(requests)
    }

    /// 同意加群请求
    pub async fn approve_request(
        &self,
        request_id: &str,
        handler_id: UserId,
    ) -> Result<JoinRequest> {
        let mut requests_guard = self.join_requests.write().await;

        let request = requests_guard.get_mut(request_id)
            .ok_or_else(|| ServerError::NotFound("加群请求不存在".to_string()))?;

        // 检查状态
        if request.status != JoinRequestStatus::Pending {
            return Err(ServerError::Validation(format!("请求状态不是待审批: {:?}", request.status)));
        }

        // 检查是否过期
        if let Some(expires_at) = request.expires_at {
            if Utc::now() > expires_at {
                request.status = JoinRequestStatus::Expired;
                request.updated_at = Utc::now();
                return Err(ServerError::Validation("请求已过期".to_string()));
            }
        }

        // 更新状态
        request.status = JoinRequestStatus::Approved;
        request.handler_id = Some(handler_id.clone());
        request.updated_at = Utc::now();

        info!("✅ 同意加群请求: request_id={}, handler_id={}", request_id, handler_id);

        Ok(request.clone())
    }

    /// 拒绝加群请求
    pub async fn reject_request(
        &self,
        request_id: &str,
        handler_id: UserId,
        reason: Option<String>,
    ) -> Result<JoinRequest> {
        let mut requests_guard = self.join_requests.write().await;

        let request = requests_guard.get_mut(request_id)
            .ok_or_else(|| ServerError::NotFound("加群请求不存在".to_string()))?;

        // 检查状态
        if request.status != JoinRequestStatus::Pending {
            return Err(ServerError::Validation(format!("请求状态不是待审批: {:?}", request.status)));
        }

        // 更新状态
        request.status = JoinRequestStatus::Rejected;
        request.handler_id = Some(handler_id.clone());
        request.reject_reason = reason;
        request.updated_at = Utc::now();

        info!("✅ 拒绝加群请求: request_id={}, handler_id={}", request_id, handler_id);

        Ok(request.clone())
    }

    /// 清理过期请求
    pub async fn cleanup_expired_requests(&self) -> Result<usize> {
        let mut requests_guard = self.join_requests.write().await;
        let now = Utc::now();
        let mut expired_count = 0;

        for request in requests_guard.values_mut() {
            if request.status == JoinRequestStatus::Pending {
                if let Some(expires_at) = request.expires_at {
                    if now > expires_at {
                        request.status = JoinRequestStatus::Expired;
                        request.updated_at = now;
                        expired_count += 1;
                    }
                }
            }
        }

        if expired_count > 0 {
            info!("🧹 清理了 {} 个过期的加群请求", expired_count);
        }

        Ok(expired_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_get_request() {
        let service = ApprovalService::new();

        let request = service.create_join_request(
            "group_123".to_string(),
            "alice".to_string(),
            JoinMethod::QRCode { qr_code_id: "qr_456".to_string() },
            Some("我想加入".to_string()),
            Some(24),
        ).await.unwrap();

        assert_eq!(request.status, JoinRequestStatus::Pending);

        let fetched = service.get_request(&request.request_id).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().user_id, "alice");
    }

    #[tokio::test]
    async fn test_approve_request() {
        let service = ApprovalService::new();

        let request = service.create_join_request(
            "group_123".to_string(),
            "alice".to_string(),
            JoinMethod::QRCode { qr_code_id: "qr_456".to_string() },
            None,
            None,
        ).await.unwrap();

        let approved = service.approve_request(&request.request_id, "admin1".to_string())
            .await.unwrap();

        assert_eq!(approved.status, JoinRequestStatus::Approved);
        assert_eq!(approved.handler_id, Some("admin1".to_string()));
    }

    #[tokio::test]
    async fn test_reject_request() {
        let service = ApprovalService::new();

        let request = service.create_join_request(
            "group_123".to_string(),
            "alice".to_string(),
            JoinMethod::MemberInvite { inviter_id: "bob".to_string() },
            None,
            None,
        ).await.unwrap();

        let rejected = service.reject_request(
            &request.request_id,
            "admin1".to_string(),
            Some("不符合要求".to_string())
        ).await.unwrap();

        assert_eq!(rejected.status, JoinRequestStatus::Rejected);
        assert_eq!(rejected.reject_reason, Some("不符合要求".to_string()));
    }

    #[tokio::test]
    async fn test_get_pending_requests_by_group() {
        let service = ApprovalService::new();

        // 创建3个请求
        service.create_join_request(
            "group_123".to_string(),
            "alice".to_string(),
            JoinMethod::QRCode { qr_code_id: "qr_1".to_string() },
            None,
            None,
        ).await.unwrap();

        service.create_join_request(
            "group_123".to_string(),
            "bob".to_string(),
            JoinMethod::QRCode { qr_code_id: "qr_2".to_string() },
            None,
            None,
        ).await.unwrap();

        let requests = service.get_pending_requests_by_group("group_123").await.unwrap();
        assert_eq!(requests.len(), 2);
    }
}

