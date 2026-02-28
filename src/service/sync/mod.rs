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

pub mod commit_dao;
pub mod pts_dao;
pub mod registry_dao;
pub mod sync_cache;
/// Phase 8: 同步服务模块
///
/// 模块组织：
/// - sync_service: 核心同步服务（RPC 处理）
/// - commit_dao: Commit Log 数据库操作
/// - pts_dao: Channel pts 数据库操作
/// - registry_dao: 客户端消息号注册表操作
/// - sync_cache: Redis 缓存操作
pub mod sync_service;

// 重新导出
pub use commit_dao::CommitLogDao;
pub use pts_dao::ChannelPtsDao;
pub use registry_dao::ClientMsgRegistryDao;
pub use sync_cache::SyncCache;
pub use sync_service::SyncService;
