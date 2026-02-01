use async_trait::async_trait;
use tracing::info;
use crate::push::provider::provider_trait::PushProvider;
use crate::push::types::{PushTask, PushVendor};
use crate::error::Result;

/// Mock Provider（用于测试和 MVP 阶段）
/// 
/// 不调用真实 API，只打印日志
pub struct MockProvider;

#[async_trait]
impl PushProvider for MockProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        info!(
            "[MOCK PUSH] Sending push: intent_id={}, task_id={}, user_id={}, device_id={}, vendor={:?}",
            task.intent_id, task.task_id, task.user_id, task.device_id, task.vendor
        );
        info!(
            "[MOCK PUSH] Payload: type={}, conversation_id={}, message_id={}, sender_id={}, preview={}",
            task.payload.r#type, 
            task.payload.conversation_id, 
            task.payload.message_id, 
            task.payload.sender_id,
            task.payload.content_preview
        );
        Ok(())
    }
    
    fn vendor(&self) -> PushVendor {
        PushVendor::Apns  // Mock 返回 Apns，实际不调用
    }
}
