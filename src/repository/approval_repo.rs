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

//! 群入群审批申请仓库（#72A）—— pending 申请持久化到 Postgres，作为真实数据源。
//! ApprovalService 只是内存缓存：启动从这里加载 pending，运行期 write-through。
//! 表结构见 `migrations/012_group_join_requests.sql`。

use crate::error::{Result, ServerError};
use crate::service::approval_service::{JoinMethod, JoinRequest, JoinRequestStatus};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;

/// 群审批申请仓库。
#[derive(Clone)]
pub struct ApprovalRepository {
    pool: Arc<PgPool>,
}

/// DB 行（原始列），再转成领域模型 `JoinRequest`。
#[derive(sqlx::FromRow)]
struct JoinRequestRow {
    request_id: String,
    group_id: i64,
    user_id: i64,
    method_type: String,
    method_ref: Option<String>,
    status: i16,
    message: Option<String>,
    handler_id: Option<i64>,
    reject_reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

fn status_to_i16(s: JoinRequestStatus) -> i16 {
    match s {
        JoinRequestStatus::Pending => 0,
        JoinRequestStatus::Approved => 1,
        JoinRequestStatus::Rejected => 2,
        JoinRequestStatus::Expired => 3,
    }
}

fn status_from_i16(v: i16) -> JoinRequestStatus {
    match v {
        1 => JoinRequestStatus::Approved,
        2 => JoinRequestStatus::Rejected,
        3 => JoinRequestStatus::Expired,
        _ => JoinRequestStatus::Pending,
    }
}

/// `JoinMethod` → (method_type, method_ref)。
fn method_to_columns(m: &JoinMethod) -> (&'static str, String) {
    match m {
        JoinMethod::MemberInvite { inviter_id } => ("member_invite", inviter_id.clone()),
        JoinMethod::QRCode { qr_code_id } => ("qrcode", qr_code_id.clone()),
    }
}

/// (method_type, method_ref) → `JoinMethod`。未知类型兜底为空 QRCode，避免 panic。
fn method_from_columns(method_type: &str, method_ref: Option<String>) -> JoinMethod {
    let r = method_ref.unwrap_or_default();
    match method_type {
        "member_invite" => JoinMethod::MemberInvite { inviter_id: r },
        _ => JoinMethod::QRCode { qr_code_id: r },
    }
}

impl From<JoinRequestRow> for JoinRequest {
    fn from(r: JoinRequestRow) -> Self {
        JoinRequest {
            request_id: r.request_id,
            group_id: r.group_id as u64,
            user_id: r.user_id as u64,
            method: method_from_columns(&r.method_type, r.method_ref),
            status: status_from_i16(r.status),
            message: r.message,
            created_at: r.created_at,
            updated_at: r.updated_at,
            expires_at: r.expires_at,
            handler_id: r.handler_id.map(|v| v as u64),
            reject_reason: r.reject_reason,
        }
    }
}

impl ApprovalRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 插入一条新申请（创建时 status=Pending）。
    pub async fn insert(&self, req: &JoinRequest) -> Result<()> {
        let (method_type, method_ref) = method_to_columns(&req.method);
        sqlx::query(
            r#"
            INSERT INTO privchat_group_join_requests
                (request_id, group_id, user_id, method_type, method_ref, status,
                 message, handler_id, reject_reason, created_at, updated_at, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ON CONFLICT (request_id) DO NOTHING
            "#,
        )
        .bind(&req.request_id)
        .bind(req.group_id as i64)
        .bind(req.user_id as i64)
        .bind(method_type)
        .bind(method_ref)
        .bind(status_to_i16(req.status))
        .bind(&req.message)
        .bind(req.handler_id.map(|v| v as i64))
        .bind(&req.reject_reason)
        .bind(req.created_at)
        .bind(req.updated_at)
        .bind(req.expires_at)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("insert join request 失败: {}", e)))?;
        Ok(())
    }

    /// 更新状态（approve / reject / expire）。写入 handler_id / reject_reason / updated_at。
    pub async fn update_status(
        &self,
        request_id: &str,
        status: JoinRequestStatus,
        handler_id: Option<u64>,
        reject_reason: Option<&str>,
        updated_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE privchat_group_join_requests
            SET status = $2, handler_id = $3, reject_reason = $4, updated_at = $5
            WHERE request_id = $1
            "#,
        )
        .bind(request_id)
        .bind(status_to_i16(status))
        .bind(handler_id.map(|v| v as i64))
        .bind(reject_reason)
        .bind(updated_at)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("update join request 失败: {}", e)))?;
        Ok(())
    }

    /// 读单条。
    pub async fn get(&self, request_id: &str) -> Result<Option<JoinRequest>> {
        let row = sqlx::query_as::<_, JoinRequestRow>(
            "SELECT * FROM privchat_group_join_requests WHERE request_id = $1",
        )
        .bind(request_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("get join request 失败: {}", e)))?;
        Ok(row.map(JoinRequest::from))
    }

    /// 启动恢复：加载所有 pending 申请（status=0）进内存缓存。
    pub async fn load_all_pending(&self) -> Result<Vec<JoinRequest>> {
        let rows = sqlx::query_as::<_, JoinRequestRow>(
            "SELECT * FROM privchat_group_join_requests WHERE status = 0",
        )
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("load pending join requests 失败: {}", e)))?;
        Ok(rows.into_iter().map(JoinRequest::from).collect())
    }

    /// 某群的 pending 申请（DB 直查，作为缓存兜底 / 校验用）。
    pub async fn list_pending_by_group(&self, group_id: u64) -> Result<Vec<JoinRequest>> {
        let rows = sqlx::query_as::<_, JoinRequestRow>(
            "SELECT * FROM privchat_group_join_requests \
             WHERE group_id = $1 AND status = 0 ORDER BY created_at DESC",
        )
        .bind(group_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("list pending by group 失败: {}", e)))?;
        Ok(rows.into_iter().map(JoinRequest::from).collect())
    }
}
