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

pub mod blacklist;
pub mod block;
pub mod friend; // ✅ 新增黑名单模块

use super::RpcServiceContext;

/// 注册联系人系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    friend::register_routes(services.clone()).await;
    block::register_routes(services.clone()).await;
    blacklist::register_routes(services.clone()).await; // ✅ 注册黑名单路由

    tracing::debug!("📋 Contact 系统路由注册完成 (friend, block, blacklist)");
}
