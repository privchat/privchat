use serde_json::{json, Value};
use crate::rpc::error::RpcResult;

/// 处理 添加黑名单 请求
pub async fn handle(body: Value) -> RpcResult<Value> {
    // TODO: 实现 添加黑名单 逻辑
    tracing::info!("🔧 处理 添加黑名单 请求: {:?}", body);
    
    // 临时返回成功响应
    Ok(json!({
        "status": "success",
        "action": "添加黑名单",
        "timestamp": chrono::Utc::now().to_rfc3339()
    }))
}
