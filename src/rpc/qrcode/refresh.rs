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

use crate::model::QRType;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 刷新 QR 码 请求
///
/// RPC: qrcode/refresh
///
/// 撤销旧的 QR Key 并生成新的
///
/// 请求参数：
/// ```json
/// {
///   "qr_type": "user",              // "user" | "group" | "auth" | "feature"
///   "target_id": "alice",           // 目标 ID
///   "creator_id": "alice"           // 创建者 ID
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "old_qr_key": "7a8b9c0d1e2f",
///   "new_qr_key": "3f4g5h6i7j8k",
///   "new_qr_code": "privchat://user/get?qrkey=3f4g5h6i7j8k",
///   "revoked_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 刷新 QR 码 请求: {:?}", body);

    // 解析参数
    let qr_type_str = body
        .get("qr_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("qr_type is required".to_string()))?;

    let qr_type = QRType::from_str(qr_type_str)
        .ok_or_else(|| RpcError::validation(format!("无效的 qr_type: {}", qr_type_str)))?;

    let target_id = body
        .get("target_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("target_id is required".to_string()))?;

    let creator_id = body
        .get("creator_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("creator_id is required".to_string()))?;

    // 刷新 QR Key
    let (old_qr_key, new_qr_key) = services
        .qrcode_service
        .refresh(target_id, qr_type, creator_id)
        .await
        .map_err(|e| RpcError::internal(format!("刷新 QR 码失败: {}", e)))?;

    // 获取新的 QR Key 记录
    let new_record = services
        .qrcode_service
        .get(&new_qr_key)
        .await
        .ok_or_else(|| RpcError::internal("无法获取新的 QR Key".to_string()))?;

    tracing::debug!(
        "✅ QR 码刷新成功: target={}, old={}, new={}",
        target_id,
        old_qr_key,
        new_qr_key
    );

    Ok(json!({
        "old_qr_key": old_qr_key,
        "new_qr_key": new_qr_key,
        "new_qr_code": new_record.to_qr_code_string(),
        "revoked_at": chrono::Utc::now().to_rfc3339(),
    }))
}
