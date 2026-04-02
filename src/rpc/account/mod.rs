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

pub mod auth;
pub mod privacy;
pub mod profile;
pub mod search;
pub mod user;

use super::RpcServiceContext;

/// 注册账户系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    user::register_routes(services.clone()).await;
    auth::register_routes(services.clone()).await; // 测试用的认证接口
    search::register_routes(services.clone()).await; // 用户搜索接口
    privacy::register_routes(services.clone()).await; // 隐私设置接口
    // TODO: 暂时注释 profile 模块
    // profile::register_routes(services.clone()).await;

    tracing::debug!("📋 Account 系统路由注册完成 (user, auth, search, privacy 模块)");
}
