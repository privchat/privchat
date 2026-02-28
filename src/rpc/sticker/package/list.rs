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

//! 获取表情包库列表 RPC 接口

use crate::rpc::error::RpcResult;
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理获取表情包库列表请求
///
/// 请求参数：（无）
///
/// 返回格式：
/// ```json
/// {
///   "packages": [
///     {
///       "package_id": "classic",
///       "name": "经典表情",
///       "thumbnail_url": "...",
///       "author": "PrivChat",
///       "description": "...",
///       "sticker_count": 10
///     }
///   ]
/// }
/// ```
pub async fn handle(services: RpcServiceContext, _params: Value) -> RpcResult<Value> {
    tracing::debug!("🔧 处理表情包库列表请求");

    // 获取所有表情包库
    let packages = services.sticker_service.list_packages().await;

    tracing::debug!("✅ 返回 {} 个表情包库", packages.len());

    Ok(json!({
        "packages": packages
    }))
}
