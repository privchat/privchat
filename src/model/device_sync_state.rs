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

//! 设备同步状态模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 设备同步状态（对应 privchat_device_sync_state 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSyncState {
    /// 用户ID
    pub user_id: Uuid,
    /// 设备ID
    pub device_id: Uuid,
    /// 会话ID
    pub channel_id: Uuid,
    /// 设备本地 pts
    pub local_pts: i64,
    /// 服务端最新 pts
    pub server_pts: i64,
    /// 最后同步时间（数据库存储为 BIGINT 毫秒时间戳）
    pub last_sync_at: DateTime<Utc>,
}

impl DeviceSyncState {
    /// 创建新的设备同步状态
    pub fn new(user_id: Uuid, device_id: Uuid, channel_id: Uuid) -> Self {
        Self {
            user_id,
            device_id,
            channel_id,
            local_pts: 0,
            server_pts: 0,
            last_sync_at: Utc::now(),
        }
    }

    /// 从数据库行创建（处理时间戳转换）
    pub fn from_db_row(
        user_id: Uuid,
        device_id: Uuid,
        channel_id: Uuid,
        local_pts: i64,
        server_pts: i64,
        last_sync_at: i64, // 毫秒时间戳
    ) -> Self {
        Self {
            user_id,
            device_id,
            channel_id,
            local_pts,
            server_pts,
            last_sync_at: DateTime::from_timestamp_millis(last_sync_at)
                .unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (Uuid, Uuid, Uuid, i64, i64, i64) {
        (
            self.user_id,
            self.device_id,
            self.channel_id,
            self.local_pts,
            self.server_pts,
            self.last_sync_at.timestamp_millis(),
        )
    }

    /// 更新同步状态
    pub fn update_sync(&mut self, server_pts: i64) {
        self.server_pts = server_pts;
        self.last_sync_at = Utc::now();
    }

    /// 计算 gap（待同步消息数）
    pub fn gap(&self) -> i64 {
        self.server_pts.saturating_sub(self.local_pts)
    }
}
