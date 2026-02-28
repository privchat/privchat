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

//! 实体同步 RPC 模块
//!
//! entity/sync_entities：按 entity_type 委托给 FriendService / ChannelService 等业务层。

mod sync_entities;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册 entity 相关路由
pub async fn register_routes(services: RpcServiceContext) {
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(
            routes::entity::SYNC_ENTITIES,
            move |body: serde_json::Value, ctx: crate::rpc::RpcContext| {
                let services = services_clone.clone();
                async move { sync_entities::handle(body, services, ctx).await }
            },
        )
        .await;

    tracing::debug!("📋 Entity 路由注册完成 (entity/sync_entities)");
}
