use crate::auth::device_manager::DeviceManager;
use crate::auth::jwt_service::JwtService;
use crate::auth::models::{
    Device, DeviceType, IssueTokenRequest, IssueTokenResponse,
};
use crate::auth::service_key_manager::ServiceKeyManager;
use crate::error::{Result, ServerError};
use chrono::Utc;
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Token 签发服务（整合所有认证服务）
pub struct TokenIssueService {
    jwt_service: Arc<JwtService>,
    service_key_manager: Arc<ServiceKeyManager>,
    device_manager: Arc<DeviceManager>,
}

impl TokenIssueService {
    /// 创建新的 Token 签发服务
    pub fn new(
        jwt_service: Arc<JwtService>,
        service_key_manager: Arc<ServiceKeyManager>,
        device_manager: Arc<DeviceManager>,
    ) -> Self {
        Self {
            jwt_service,
            service_key_manager,
            device_manager,
        }
    }
    
    /// Token 签发接口（业务系统调用）
    pub async fn issue_token(
        &self,
        service_key: &str,
        request: IssueTokenRequest,
        ip_address: String,
    ) -> Result<IssueTokenResponse> {
        debug!(
            "收到 token 签发请求: user_id={}, business_system_id={}, app_id={}",
            request.user_id, request.business_system_id, request.device_info.app_id
        );
        
        // 1. 验证 service key
        if !self.service_key_manager.verify(service_key).await {
            warn!(
                "❌ 无效的 service key，拒绝签发 token (business_system_id: {})",
                request.business_system_id
            );
            return Err(ServerError::Unauthorized("无效的 service key".to_string()));
        }
        
        // 2. 验证请求参数
        Self::validate_request(&request)?;
        
        // 3. 处理设备ID：如果客户端提供则使用，否则自动生成
        let device_id = if let Some(ref client_device_id) = request.device_id {
            // 验证客户端提供的 device_id 必须是 UUID 格式
            Uuid::parse_str(client_device_id)
                .map_err(|_| ServerError::Validation(
                    "device_id 必须是有效的 UUID 格式".to_string()
                ))?;
            client_device_id.clone()
        } else {
            // 客户端未提供，服务器自动生成
            Uuid::new_v4().to_string()
        };
        
        // 4. 签发 IM token
        let im_token = self.jwt_service.issue_token(
            request.user_id,
            &device_id,
            &request.business_system_id,
            &request.device_info.app_id,
            request.ttl,
        )?;
        
        // 5. 解析 token 获取 claims (提取 jti)
        let claims = self.jwt_service.verify_token(&im_token)?;
        
        // 6. 注册设备
        let device = Device {
            device_id: device_id.clone(),
            user_id: request.user_id,
            business_system_id: request.business_system_id.clone(),
            device_info: request.device_info.clone(),
            device_type: DeviceType::from_app_id(&request.device_info.app_id),
            token_jti: claims.jti,
            session_version: 1,  // ✨ 新增
            session_state: crate::auth::SessionState::Active,  // ✨ 新增
            kicked_at: None,  // ✨ 新增
            kicked_reason: None,  // ✨ 新增
            last_active_at: Utc::now(),
            created_at: Utc::now(),
            ip_address: ip_address.clone(),
        };
        
        self.device_manager.register_device(device).await?;
        
        let ttl = request.ttl.unwrap_or(self.jwt_service.default_ttl());
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl);
        
        info!(
            "✅ Token 签发成功: user_id={}, device_id={}, business_system={}",
            request.user_id, device_id, request.business_system_id
        );
        
        Ok(IssueTokenResponse {
            im_token,
            device_id,
            expires_in: ttl,
            expires_at,
        })
    }
    
    /// 验证请求参数
    fn validate_request(request: &IssueTokenRequest) -> Result<()> {
        // 验证 user_id
        if request.user_id == 0 {
            return Err(ServerError::Validation("user_id 不能为 0".to_string()));
        }
        
        // device_id 的验证在 issue_token 中处理（需要解析 UUID）
        
        // 验证 business_system_id
        if request.business_system_id.is_empty() {
            return Err(ServerError::Validation(
                "business_system_id 不能为空".to_string(),
            ));
        }
        
        if request.business_system_id.len() > 255 {
            return Err(ServerError::Validation(
                "business_system_id 长度不能超过 255".to_string(),
            ));
        }
        
        // 验证 device_info
        if request.device_info.app_id.is_empty() {
            return Err(ServerError::Validation("app_id 不能为空".to_string()));
        }
        
        if request.device_info.device_name.is_empty() {
            return Err(ServerError::Validation(
                "device_name 不能为空".to_string(),
            ));
        }
        
        // 验证 ttl (如果提供)
        if let Some(ttl) = request.ttl {
            if ttl < 60 {
                return Err(ServerError::Validation(
                    "ttl 不能小于 60 秒".to_string(),
                ));
            }
            
            if ttl > 30 * 24 * 3600 {
                // 最大 30 天
                return Err(ServerError::Validation(
                    "ttl 不能超过 30 天".to_string(),
                ));
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::models::DeviceInfo;
    
    #[tokio::test]
    async fn test_issue_token() {
        let jwt_service = Arc::new(JwtService::new(
            "test-secret-key-at-least-32-chars".to_string(),
            3600,
        ));
        
        let service_key_manager =
            Arc::new(ServiceKeyManager::new_master_key("test-key".to_string()));
        
        let device_manager = Arc::new(DeviceManager::new());
        
        let service = TokenIssueService::new(
            jwt_service,
            service_key_manager,
            device_manager.clone(),
        );
        
        let request = IssueTokenRequest {
            user_id: 12345,
            business_system_id: "ecommerce".to_string(),
            device_id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            device_info: DeviceInfo {
                app_id: "ios".to_string(),
                device_name: "iPhone".to_string(),
                device_model: "iPhone 15".to_string(),
                os_version: "iOS 17".to_string(),
                app_version: "1.0.0".to_string(),
            },
            ttl: None,
        };
        
        let response = service
            .issue_token("test-key", request, "127.0.0.1".to_string())
            .await
            .unwrap();
        
        assert!(!response.im_token.is_empty());
        assert!(!response.device_id.is_empty());
        assert_eq!(response.expires_in, 3600);
        
        // 验证设备已注册
        let device = device_manager.get_device(&response.device_id).await;
        assert!(device.is_some());
    }
    
    #[tokio::test]
    async fn test_issue_token_invalid_service_key() {
        let jwt_service = Arc::new(JwtService::new(
            "test-secret-key-at-least-32-chars".to_string(),
            3600,
        ));
        
        let service_key_manager =
            Arc::new(ServiceKeyManager::new_master_key("correct-key".to_string()));
        
        let device_manager = Arc::new(DeviceManager::new());
        
        let service = TokenIssueService::new(jwt_service, service_key_manager, device_manager);
        
        let request = IssueTokenRequest {
            user_id: 12345,
            business_system_id: "ecommerce".to_string(),
            device_id: Some("550e8400-e29b-41d4-a716-446655440001".to_string()),
            device_info: DeviceInfo {
                app_id: "ios".to_string(),
                device_name: "iPhone".to_string(),
                device_model: "iPhone 15".to_string(),
                os_version: "iOS 17".to_string(),
                app_version: "1.0.0".to_string(),
            },
            ttl: None,
        };
        
        let result = service
            .issue_token("wrong-key", request, "127.0.0.1".to_string())
            .await;
        
        assert!(result.is_err());
    }
}

