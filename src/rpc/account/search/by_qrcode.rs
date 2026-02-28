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

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use privchat_protocol::rpc::account::search::AccountSearchByQRCodeRequest;
use serde_json::{json, Value};

/// 处理 扫码查找用户 请求（精确搜索）
///
/// 通过二维码精确查找用户，用于扫码添加好友等场景
/// 返回 search_session_id，用于后续查看用户详情
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理扫码查找用户请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: AccountSearchByQRCodeRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 searcher_id
    request.searcher_id = crate::rpc::get_current_user_id(&ctx)?;

    let searcher_id = request.searcher_id;
    let qrcode = &request.qr_key; // Protocol 中字段名是 qr_key

    // 通过 qrcode 索引查找 user_id
    match services.cache_manager.find_user_by_qrcode(qrcode).await {
        Ok(Some(user_id)) => {
            // 检查隐私设置：是否允许通过二维码搜索
            let privacy = services
                .privacy_service
                .get_or_create_privacy_settings(user_id)
                .await
                .map_err(|e| {
                    RpcError::internal(format!("Failed to get privacy settings: {}", e))
                })?;

            if !privacy.allow_search_by_qrcode {
                return Err(RpcError::forbidden(
                    "User does not allow being searched by qrcode".to_string(),
                ));
            }

            // 创建搜索记录（用于后续查看详情）
            let search_record = services
                .cache_manager
                .create_search_record(searcher_id, user_id)
                .await
                .map_err(|e| {
                    RpcError::internal(format!("Failed to create search record: {}", e))
                })?;

            // 通过 user_id 获取用户资料（从数据库读取）
            match helpers::get_user_profile_with_fallback(
                user_id,
                &services.user_repository,
                &services.cache_manager,
            )
            .await
            {
                Ok(Some(user_profile)) => {
                    tracing::debug!("✅ 扫码查找成功: qrcode={} -> user_id={}", qrcode, user_id);
                    Ok(json!({
                        "user_id": user_profile.user_id,
                        "username": user_profile.username,
                        "nickname": user_profile.nickname,
                        "avatar_url": user_profile.avatar_url,
                        "search_session_id": search_record.search_session_id, // 用于后续查看详情
                    }))
                }
                Ok(None) => {
                    tracing::warn!(
                        "⚠️ qrcode 索引存在但用户资料不存在: qrcode={}, user_id={}",
                        qrcode,
                        user_id
                    );
                    Err(RpcError::not_found(format!(
                        "User profile not found for qrcode: {}",
                        qrcode
                    )))
                }
                Err(e) => {
                    tracing::error!("❌ 获取用户资料失败: {}", e);
                    Err(RpcError::internal("Database error".to_string()))
                }
            }
        }
        Ok(None) => {
            tracing::debug!("❌ 未找到对应的用户: qrcode={}", qrcode);
            Err(RpcError::not_found(format!(
                "User not found for qrcode: {}",
                qrcode
            )))
        }
        Err(e) => {
            tracing::error!("❌ 查找 qrcode 失败: {}", e);
            Err(RpcError::internal("Database error".to_string()))
        }
    }
}
