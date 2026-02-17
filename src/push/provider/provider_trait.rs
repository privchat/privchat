use crate::error::Result;
use crate::push::types::PushVendor;
use async_trait::async_trait;

// 前向声明，避免循环依赖
use crate::push::types::PushTask;

/// Push Provider Trait（推送提供者接口）
///
/// MVP 阶段最小化实现
#[async_trait]
pub trait PushProvider: Send + Sync {
    /// 发送推送
    async fn send(&self, task: &PushTask) -> Result<()>;

    /// 获取 Provider 对应的 Vendor
    fn vendor(&self) -> PushVendor;
}
