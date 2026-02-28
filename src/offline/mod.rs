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

// 离线消息系统模块
// 负责处理用户离线时的消息存储、投递和管理

pub mod message;
pub mod queue;
pub mod scheduler;
pub mod storage;

// 重新导出主要类型
pub use message::{DeliveryStatus, MessagePriority, OfflineMessage};
pub use queue::{OfflineQueueManager, QueueStats};
pub use scheduler::{OfflineScheduler, SchedulerConfig};
pub use storage::{SledStorage, StorageBackend};
