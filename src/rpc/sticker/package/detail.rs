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

//! 获取表情包库详情 RPC 接口

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理获取表情包库详情请求
///
/// 请求参数：
/// ```json
/// {
///   "package_id": "classic"
/// }
/// ```
///
/// 返回格式：
/// ```json
/// {
///   "package": {
///     "package_id": "classic",
///     "name": "经典表情",
///     "thumbnail_url": "...",
///     "author": "PrivChat",
///     "description": "...",
///     "sticker_count": 10,
///     "stickers": [
///       {
///         "sticker_id": "classic_001",
///         "package_id": "classic",
///         "image_url": "...",
///         "alt_text": "微笑",
///         "emoji": "😊",
///         "width": 128,
///         "height": 128,
///         "mime_type": "image/png"
///       }
///     ]
///   }
/// }
/// ```
pub async fn handle(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    tracing::debug!("🔧 处理表情包库详情请求: {:?}", params);

    // 提取 package_id
    let package_id = params["package_id"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 package_id 参数".to_string()))?;

    // 获取表情包库详情
    let package = services
        .sticker_service
        .get_package_detail(package_id)
        .await
        .ok_or_else(|| RpcError::not_found(format!("表情包库 {} 不存在", package_id)))?;

    tracing::debug!("✅ 返回表情包库详情: {}", package_id);

    Ok(json!({
        "package": package
    }))
}
