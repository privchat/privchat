use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use serde_json::Value;
use tokio::sync::RwLock;

use super::error::RpcResult;
use super::types::{RPCMessageRequest, RPCMessageResponse};

/// RPC 处理函数类型
pub type RpcHandler = Arc<dyn Fn(Value, super::RpcContext) -> Pin<Box<dyn Future<Output = RpcResult<Value>> + Send>> + Send + Sync>;

/// RPC 路由器
#[derive(Clone)]
pub struct RpcRouter {
    routes: Arc<RwLock<HashMap<String, RpcHandler>>>,
}

impl RpcRouter {
    /// 创建新的路由器
    pub fn new() -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 注册路由
    pub async fn register<F, Fut>(&self, route: &str, handler: F)
    where
        F: Fn(Value, super::RpcContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = RpcResult<Value>> + Send + 'static,
    {
        let handler: RpcHandler = Arc::new(move |body, ctx| Box::pin(handler(body, ctx)));
        self.routes.write().await.insert(route.to_string(), handler);
    }

    /// 处理 RPC 请求
    pub async fn handle(&self, request: RPCMessageRequest, ctx: super::RpcContext) -> RPCMessageResponse {
        let routes = self.routes.read().await;
        
        if let Some(handler) = routes.get(&request.route) {
            match handler(request.body, ctx).await {
                Ok(data) => RPCMessageResponse::success(data),
                Err(e) => RPCMessageResponse::error(e.code_value() as i32, e.message().to_string()),
            }
        } else {
            RPCMessageResponse::error(
                privchat_protocol::ErrorCode::ResourceNotFound.code() as i32,
                format!("Route '{}' not found", request.route)
            )
        }
    }

    /// 获取所有注册的路由
    pub async fn list_routes(&self) -> Vec<String> {
        self.routes.read().await.keys().cloned().collect()
    }
}

impl Default for RpcRouter {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static::lazy_static! {
    pub static ref GLOBAL_RPC_ROUTER: RpcRouter = RpcRouter::new();
} 