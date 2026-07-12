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

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

use crate::error::{Result, ServerError};
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
    pub message: Option<String>, // 申请消息
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub handler_id: Option<UserId>,    // 处理人ID
    pub reject_reason: Option<String>, // 拒绝原因
}

/// 加群审批服务
///
/// #72A：申请数据的**真实数据源是 Postgres**（`ApprovalRepository`），本 service 只是
/// 内存缓存 —— 启动 `load_pending()` 从 DB 恢复所有 pending，运行期所有写操作 DB-first
/// （先落库成功再改缓存），因此 server 重启 / crash / 滚动升级不再丢待审批申请。
pub struct ApprovalService {
    /// 持久化仓库（真实数据源）。
    repo: Arc<crate::repository::ApprovalRepository>,
    /// 缓存：所有加群请求，key: request_id
    join_requests: Arc<RwLock<HashMap<String, JoinRequest>>>,
    /// 索引：按群组ID索引请求，key: group_id, value: Vec<request_id>
    group_requests_index: Arc<RwLock<HashMap<ChannelId, Vec<String>>>>,
    /// 索引：按用户ID索引请求，key: user_id, value: Vec<request_id>
    user_requests_index: Arc<RwLock<HashMap<UserId, Vec<String>>>>,
}

impl ApprovalService {
    pub fn new(repo: Arc<crate::repository::ApprovalRepository>) -> Self {
        Self {
            repo,
            join_requests: Arc::new(RwLock::new(HashMap::new())),
            group_requests_index: Arc::new(RwLock::new(HashMap::new())),
            user_requests_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 启动恢复（#72A）：从 DB 加载所有 pending 申请进缓存 + 重建索引。
    /// server 每次启动调用一次；无 pending 时 no-op。
    pub async fn load_pending(&self) -> Result<usize> {
        let pending = self.repo.load_all_pending().await?;
        let n = pending.len();
        let mut jr = self.join_requests.write().await;
        let mut gi = self.group_requests_index.write().await;
        let mut ui = self.user_requests_index.write().await;
        for req in pending {
            gi.entry(req.group_id).or_default().push(req.request_id.clone());
            ui.entry(req.user_id).or_default().push(req.request_id.clone());
            jr.insert(req.request_id.clone(), req);
        }
        info!("🔄 群审批启动恢复：从 DB 加载 {} 条 pending 申请进缓存", n);
        Ok(n)
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

        // #72A：DB-first —— 先落库（真实数据源），失败则不进缓存，保证一致。
        self.repo.insert(&request).await?;

        // 存储请求（缓存）
        self.join_requests
            .write()
            .await
            .insert(request_id.clone(), request.clone());

        // 更新索引
        self.group_requests_index
            .write()
            .await
            .entry(group_id.clone())
            .or_default()
            .push(request_id.clone());

        self.user_requests_index
            .write()
            .await
            .entry(user_id.clone())
            .or_default()
            .push(request_id.clone());

        info!(
            "✅ 创建加群请求: request_id={}, group_id={}, user_id={}",
            request_id, group_id, user_id
        );

        Ok(request)
    }

    /// 获取单个加群请求
    pub async fn get_request(&self, request_id: &str) -> Result<Option<JoinRequest>> {
        Ok(self.join_requests.read().await.get(request_id).cloned())
    }

    /// 获取群组的所有待审批请求
    pub async fn get_pending_requests_by_group(&self, group_id: u64) -> Result<Vec<JoinRequest>> {
        let request_ids = self
            .group_requests_index
            .read()
            .await
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
    pub async fn get_requests_by_user(&self, user_id: u64) -> Result<Vec<JoinRequest>> {
        let request_ids = self
            .user_requests_index
            .read()
            .await
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

        // 先只读校验（immutable peek，借用在块内结束，不跨 await）。
        let (cur_status, is_expired) = match requests_guard.get(request_id) {
            Some(r) => (
                r.status,
                r.expires_at.map(|e| Utc::now() > e).unwrap_or(false),
            ),
            None => return Err(ServerError::NotFound("加群请求不存在".to_string())),
        };
        if cur_status != JoinRequestStatus::Pending {
            return Err(ServerError::Validation(format!(
                "请求状态不是待审批: {:?}",
                cur_status
            )));
        }

        let now = Utc::now();
        if is_expired {
            // #72A：过期状态也落库，避免重启后又变回 pending。
            self.repo
                .update_status(request_id, JoinRequestStatus::Expired, None, None, now)
                .await?;
            if let Some(r) = requests_guard.get_mut(request_id) {
                r.status = JoinRequestStatus::Expired;
                r.updated_at = now;
            }
            return Err(ServerError::Validation("请求已过期".to_string()));
        }

        // #72A：DB-first —— 先落库 approved，成功再改缓存。
        self.repo
            .update_status(
                request_id,
                JoinRequestStatus::Approved,
                Some(handler_id),
                None,
                now,
            )
            .await?;

        let request = requests_guard
            .get_mut(request_id)
            .ok_or_else(|| ServerError::NotFound("加群请求不存在".to_string()))?;
        request.status = JoinRequestStatus::Approved;
        request.handler_id = Some(handler_id);
        request.updated_at = now;

        info!(
            "✅ 同意加群请求: request_id={}, handler_id={}",
            request_id, handler_id
        );

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

        let cur_status = match requests_guard.get(request_id) {
            Some(r) => r.status,
            None => return Err(ServerError::NotFound("加群请求不存在".to_string())),
        };
        if cur_status != JoinRequestStatus::Pending {
            return Err(ServerError::Validation(format!(
                "请求状态不是待审批: {:?}",
                cur_status
            )));
        }

        let now = Utc::now();
        // #72A：DB-first —— 先落库 rejected（含 reason），成功再改缓存。
        self.repo
            .update_status(
                request_id,
                JoinRequestStatus::Rejected,
                Some(handler_id),
                reason.as_deref(),
                now,
            )
            .await?;

        let request = requests_guard
            .get_mut(request_id)
            .ok_or_else(|| ServerError::NotFound("加群请求不存在".to_string()))?;
        request.status = JoinRequestStatus::Rejected;
        request.handler_id = Some(handler_id);
        request.reject_reason = reason;
        request.updated_at = now;

        info!(
            "✅ 拒绝加群请求: request_id={}, handler_id={}",
            request_id, handler_id
        );

        Ok(request.clone())
    }

    /// 清理过期请求（#72A：过期状态也落库）。
    pub async fn cleanup_expired_requests(&self) -> Result<usize> {
        let now = Utc::now();
        // 只读收集需要过期的 request_id。
        let ids: Vec<String> = {
            let guard = self.join_requests.read().await;
            guard
                .values()
                .filter(|r| {
                    r.status == JoinRequestStatus::Pending
                        && r.expires_at.map(|e| now > e).unwrap_or(false)
                })
                .map(|r| r.request_id.clone())
                .collect()
        };
        // DB-first：逐条落库过期。
        for id in &ids {
            self.repo
                .update_status(id, JoinRequestStatus::Expired, None, None, now)
                .await?;
        }
        // 再改缓存。
        if !ids.is_empty() {
            let mut guard = self.join_requests.write().await;
            for id in &ids {
                if let Some(r) = guard.get_mut(id) {
                    r.status = JoinRequestStatus::Expired;
                    r.updated_at = now;
                }
            }
            info!("🧹 清理了 {} 个过期的加群请求", ids.len());
        }
        Ok(ids.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository::ApprovalRepository;
    use sqlx::postgres::PgPoolOptions;

    /// #72A：这些测试需要一个可写的 Postgres（含 privchat_group_join_requests 表）。
    /// 设 `TEST_DATABASE_URL` 才运行；未设则跳过（`cargo test` 不会失败）。
    /// 建库后跑：`TEST_DATABASE_URL=postgres://... cargo test approval_service`。
    async fn test_service() -> Option<(ApprovalService, Arc<ApprovalRepository>)> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .ok()?;
        let pool = Arc::new(pool);
        let repo = Arc::new(ApprovalRepository::new(pool));
        Some((ApprovalService::new(repo.clone()), repo))
    }

    fn uniq_group() -> u64 {
        // 用大随机 group_id 避免与真实数据/并发测试冲突。
        9_900_000_000 + (std::process::id() as u64) * 1000 + (line!() as u64)
    }

    #[tokio::test]
    async fn test_create_persists_and_survives_restart() {
        let Some((service, repo)) = test_service().await else {
            eprintln!("跳过：未设 TEST_DATABASE_URL");
            return;
        };
        let group = uniq_group();
        let req = service
            .create_join_request(
                group,
                1001,
                JoinMethod::QRCode { qr_code_id: "qr_x".into() },
                Some("我想加入".into()),
                Some(24),
            )
            .await
            .unwrap();
        assert_eq!(req.status, JoinRequestStatus::Pending);

        // 模拟 server 重启：全新 service（空缓存）→ load_pending 从 DB 恢复。
        let fresh = ApprovalService::new(repo.clone());
        fresh.load_pending().await.unwrap();
        let pending = fresh.get_pending_requests_by_group(group).await.unwrap();
        assert_eq!(pending.len(), 1, "重启后 pending 应从 DB 恢复");
        assert_eq!(pending[0].user_id, 1001);
    }

    #[tokio::test]
    async fn test_approve_persists_and_disappears_after_restart() {
        let Some((service, repo)) = test_service().await else {
            return;
        };
        let group = uniq_group();
        let req = service
            .create_join_request(group, 1002, JoinMethod::QRCode { qr_code_id: "qr_y".into() }, None, None)
            .await
            .unwrap();
        let approved = service.approve_request(&req.request_id, 9001).await.unwrap();
        assert_eq!(approved.status, JoinRequestStatus::Approved);

        // 重启后不应再是 pending（DB 已 UPDATE status=approved）。
        let fresh = ApprovalService::new(repo.clone());
        fresh.load_pending().await.unwrap();
        assert_eq!(fresh.get_pending_requests_by_group(group).await.unwrap().len(), 0);
        assert_eq!(
            repo.get(&req.request_id).await.unwrap().unwrap().status,
            JoinRequestStatus::Approved
        );
    }

    #[tokio::test]
    async fn test_reject_persists_reason() {
        let Some((service, repo)) = test_service().await else {
            return;
        };
        let group = uniq_group();
        let req = service
            .create_join_request(group, 1003, JoinMethod::MemberInvite { inviter_id: "bob".into() }, None, None)
            .await
            .unwrap();
        let rejected = service
            .reject_request(&req.request_id, 9001, Some("不符合要求".into()))
            .await
            .unwrap();
        assert_eq!(rejected.status, JoinRequestStatus::Rejected);

        let persisted = repo.get(&req.request_id).await.unwrap().unwrap();
        assert_eq!(persisted.status, JoinRequestStatus::Rejected);
        assert_eq!(persisted.reject_reason, Some("不符合要求".into()));
    }
}
