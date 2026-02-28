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

use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::context::RequestContext;
use crate::error::Result;
use crate::handler::MessageHandler;
use crate::middleware::AuthMiddleware;
use crate::rpc::{handle_rpc_request, types::RPCMessageRequest};

/// RPC 消息处理器
pub struct RPCMessageHandler {
    auth_middleware: Arc<AuthMiddleware>,
}

impl RPCMessageHandler {
    pub fn new(auth_middleware: Arc<AuthMiddleware>) -> Self {
        Self { auth_middleware }
    }
}

#[async_trait]
impl MessageHandler for RPCMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        debug!("🔧 处理 RPC 请求: session_id={}", context.session_id);

        // 解析 RPC 请求
        let rpc_request: RPCMessageRequest = match serde_json::from_slice(&context.data) {
            Ok(req) => req,
            Err(e) => {
                tracing::error!("❌ RPC 请求解析失败: {}", e);
                let error_response = serde_json::json!({
                    "code": 400,
                    "message": "Invalid RPC request format",
                    "data": null
                });
                return Ok(Some(serde_json::to_vec(&error_response)?));
            }
        };

        info!(
            "🚀 RPC 调用: route={}, session_id={}",
            rpc_request.route, context.session_id
        );

        // 🔐 认证检查并构建 RPC 上下文
        let rpc_context = match self
            .auth_middleware
            .check_rpc_route(&rpc_request.route, &context.session_id)
            .await
        {
            Ok(Some(user_id)) => {
                // 已认证，可以继续
                info!(
                    "🔐 RPC 认证成功: route={}, user={}",
                    rpc_request.route, user_id
                );
                crate::rpc::RpcContext::new()
                    .with_user_id(user_id)
                    .with_device_id(context.device_id.clone().unwrap_or_default())
                    .with_session_id(context.session_id.to_string())
            }
            Ok(None) => {
                // 匿名访问（白名单）
                info!("🌐 RPC 匿名访问（白名单）: route={}", rpc_request.route);
                crate::rpc::RpcContext::new().with_session_id(context.session_id.to_string())
            }
            Err(error_code) => {
                // 认证失败，返回错误码
                warn!(
                    "❌ RPC 认证失败: route={}, error={}",
                    rpc_request.route, error_code
                );
                let error_response = serde_json::json!({
                    "code": error_code.code(),
                    "message": error_code.message(),
                    "data": null
                });
                return Ok(Some(serde_json::to_vec(&error_response)?));
            }
        };

        // 处理 RPC 请求
        let start = std::time::Instant::now();
        let rpc_response = handle_rpc_request(rpc_request.clone(), rpc_context).await;
        let elapsed = start.elapsed().as_secs_f64();
        crate::infra::metrics::record_rpc(&rpc_request.route, elapsed);

        info!(
            "✅ RPC 调用完成: route={}, code={}, message={}",
            rpc_request.route, rpc_response.code, rpc_response.message
        );

        // 序列化响应
        let response_data = serde_json::to_vec(&rpc_response)?;
        Ok(Some(response_data))
    }

    fn name(&self) -> &'static str {
        "RPCMessageHandler"
    }
}
