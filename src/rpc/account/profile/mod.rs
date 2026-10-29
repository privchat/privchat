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

pub mod get;
pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use privchat_protocol::rpc::routes;

/// 注册个人资料模块的所有路由
pub async fn register_routes() {
    GLOBAL_RPC_ROUTER
        .register(routes::account_profile::GET, get::handle)
        .await;
    GLOBAL_RPC_ROUTER
        .register(routes::account_profile::UPDATE, update::handle)
        .await;

    tracing::debug!("📋 Profile 模块路由注册完成");
}
