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

//! 黑名单模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 黑名单（对应 privchat_blacklist 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blacklist {
    /// 用户ID
    pub user_id: Uuid,
    /// 被拉黑的用户ID
    pub blocked_user_id: Uuid,
    /// 拉黑时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
}

impl Blacklist {
    /// 创建新的黑名单记录
    pub fn new(user_id: Uuid, blocked_user_id: Uuid) -> Self {
        Self {
            user_id,
            blocked_user_id,
            created_at: Utc::now(),
        }
    }

    /// 从数据库行创建（处理时间戳转换）
    pub fn from_db_row(
        user_id: Uuid,
        blocked_user_id: Uuid,
        created_at: i64, // 毫秒时间戳
    ) -> Self {
        Self {
            user_id,
            blocked_user_id,
            created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (Uuid, Uuid, i64) {
        (
            self.user_id,
            self.blocked_user_id,
            self.created_at.timestamp_millis(),
        )
    }
}
