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
use std::time::Duration;
use tracing::{debug, info, warn};

use crate::context::RequestContext;
use crate::error::Result;
use crate::handler::MessageHandler;
use crate::infra::ConnectionManager;
use crate::middleware::AuthMiddleware;
use crate::rpc::{handle_rpc_request, types::RPCMessageRequest};

/// RPC 消息处理器
pub struct RPCMessageHandler {
    auth_middleware: Arc<AuthMiddleware>,
    connection_manager: Arc<ConnectionManager>,
}

impl RPCMessageHandler {
    pub fn new(
        auth_middleware: Arc<AuthMiddleware>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            auth_middleware,
            connection_manager,
        }
    }
}

#[async_trait]
impl MessageHandler for RPCMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        debug!("🔧 处理 RPC 请求: session_id={}", context.session_id);

        // Wire format is FlatBuffers privchat_protocol::RpcRequest. The
        // protocol envelope carries `route` plus opaque body bytes (currently
        // JSON UTF-8 of the RPC params); we parse the body into a
        // serde_json::Value for the existing handler dispatcher.
        let fb_request: privchat_protocol::RpcRequest =
            match privchat_protocol::decode_message(&context.data) {
                Ok(req) => req,
                Err(e) => {
                    tracing::error!("❌ RPC 请求解析失败 (flatbuffers): {}", e);
                    return Ok(Some(encode_rpc_error(400, "Invalid RPC request format")?));
                }
            };

        let body_value: serde_json::Value = if fb_request.body.is_empty() {
            serde_json::Value::Null
        } else {
            match serde_json::from_slice(&fb_request.body) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("❌ RPC body JSON 解析失败: {}", e);
                    return Ok(Some(encode_rpc_error(400, "Invalid RPC body JSON")?));
                }
            }
        };

        let rpc_request = RPCMessageRequest {
            route: fb_request.route,
            body: body_value,
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
                // 认证失败：先把错误码回给客户端，再主动断开这条 session。
                //
                // 未认证 session 继续存活只会让客户端误以为通道可用，把队列里的
                // 消息/文件 token 重复投递过来打满日志；主动断开能让客户端走
                // reconnect + re-authenticate 通道恢复，与 SDK 的 auto-reconnect
                // 形成闭环。
                warn!(
                    "❌ RPC 认证失败，将断开 session: route={}, session={}, error={}",
                    rpc_request.route, context.session_id, error_code
                );
                let response_bytes = encode_rpc_error(error_code.code() as i32, error_code.message())?;

                // 等错误响应发出后再断开（~50ms 足够 transport 把帧刷到网络）。
                let cm = self.connection_manager.clone();
                let sid = context.session_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    cm.force_close_session(sid).await;
                });

                return Ok(Some(response_bytes));
            }
        };

        // 处理 RPC 请求
        let start = std::time::Instant::now();
        let rpc_response = handle_rpc_request(rpc_request.clone(), rpc_context).await;
        let elapsed = start.elapsed().as_secs_f64();
        crate::infra::metrics::record_rpc(&rpc_request.route, elapsed);
        // 真实 handler 结果（ErrorCode::Ok=0=ok，否则 error）；供 G10 §3.1 handler 错误率。
        crate::infra::metrics::record_handler_result(if rpc_response.code == 0 { "ok" } else { "error" });

        info!(
            "✅ RPC 调用完成: route={}, code={}, message={}",
            rpc_request.route, rpc_response.code, rpc_response.message
        );

        // Encode response as FlatBuffers privchat_protocol::RpcResponse.
        // The original RpcResponse.data is serde_json::Value — wrap it as
        // JSON UTF-8 bytes inside the FB envelope's `data: [ubyte]` slot.
        let data_bytes = match &rpc_response.data {
            Some(v) => {
                let bytes = serde_json::to_vec(v)?;
                if bytes.is_empty() { None } else { Some(bytes) }
            }
            None => None,
        };
        let fb_response = privchat_protocol::RpcResponse {
            code: rpc_response.code,
            message: rpc_response.message.clone(),
            data: data_bytes,
        };
        let response_bytes = privchat_protocol::encode_message(&fb_response)
            .map_err(|e| crate::error::ServerError::Internal(format!("encode rpc response: {e}")))?;
        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "RPCMessageHandler"
    }
}

/// Encode an RPC-layer error into a FlatBuffers `RpcResponse`.
fn encode_rpc_error(code: i32, message: &str) -> Result<Vec<u8>> {
    let resp = privchat_protocol::RpcResponse {
        code,
        message: message.to_string(),
        data: None,
    };
    privchat_protocol::encode_message(&resp)
        .map_err(|e| crate::error::ServerError::Internal(format!("encode rpc error: {e}")).into())
}
