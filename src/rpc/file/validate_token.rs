//! RPC: 验证上传 token（内部 RPC）
//! 
//! 由文件服务器调用，验证上传 token 的有效性

use serde_json::{json, Value};
use tracing::{info, warn};

use crate::rpc::{RpcServiceContext, RpcResult, RpcError};

/// 验证上传 token
pub async fn validate_upload_token(
    services: RpcServiceContext,
    params: Value,
) -> RpcResult<Value> {
    // 解析参数
    let upload_token = params["upload_token"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 upload_token 参数".to_string()))?;
    
    info!("🔐 验证上传 token: {}", upload_token);
    
    // 验证 token
    match services.upload_token_service.validate_token(upload_token).await {
        Ok(token_info) => {
            // Token 有效，标记为已使用
            services.upload_token_service
                .mark_token_used(upload_token)
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?;
            
            Ok(json!({
                "valid": true,
                "user_id": token_info.user_id,
                "file_type": token_info.file_type.as_str(),
                "max_size": token_info.max_size,
                "business_type": token_info.business_type,
            }))
        }
        Err(e) => {
            warn!("❌ Token 验证失败: {}", upload_token);
            Ok(json!({
                "valid": false,
                "error": e.to_string(),
            }))
        }
    }
}

