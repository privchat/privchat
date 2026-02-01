use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, error, debug, warn};
use uuid::Uuid;
use crate::push::types::{PushIntent, PushTask, PushVendor, IntentStatus};
use crate::push::provider::{PushProvider, MockProvider, FcmProvider, ApnsProvider};
use crate::push::intent_state::IntentStateManager;
use crate::repository::UserDeviceRepository;
use crate::error::Result;

/// Push Worker（推送工作器）
/// 
/// 职责：
/// - 从内存队列接收 PushIntent
/// - 查询用户设备列表
/// - 展开 Intent 为设备级 PushTask
/// - 调用 Provider 发送推送
/// - 检查 Intent 状态（撤销/取消）
pub struct PushWorker {
    receiver: mpsc::Receiver<PushIntent>,
    mock_provider: Arc<MockProvider>,
    fcm_provider: Option<Arc<FcmProvider>>,  // Phase 2: FCM Provider（可选）
    apns_provider: Option<Arc<ApnsProvider>>,  // Phase 3: APNs Provider（可选）
    device_repo: Option<Arc<UserDeviceRepository>>,
    intent_state: Option<Arc<IntentStateManager>>,  // Phase 3: Intent 状态管理器
}

impl PushWorker {
    pub fn new(receiver: mpsc::Receiver<PushIntent>) -> Self {
        Self {
            receiver,
            mock_provider: Arc::new(MockProvider),
            fcm_provider: None,
            apns_provider: None,
            device_repo: None,
            intent_state: None,
        }
    }
    
    /// 创建带设备 Repository 的 Worker（Phase 2）
    pub fn with_device_repo(receiver: mpsc::Receiver<PushIntent>, device_repo: Arc<UserDeviceRepository>) -> Self {
        Self {
            receiver,
            mock_provider: Arc::new(MockProvider),
            fcm_provider: None,
            apns_provider: None,
            device_repo: Some(device_repo),
            intent_state: None,
        }
    }
    
    /// 创建带 Provider 和状态管理器的 Worker（Phase 3）
    pub fn with_providers(
        receiver: mpsc::Receiver<PushIntent>,
        device_repo: Arc<UserDeviceRepository>,
        intent_state: Arc<IntentStateManager>,
        fcm_provider: Option<Arc<FcmProvider>>,
        apns_provider: Option<Arc<ApnsProvider>>,
    ) -> Self {
        Self {
            receiver,
            mock_provider: Arc::new(MockProvider),
            fcm_provider,
            apns_provider,
            device_repo: Some(device_repo),
            intent_state: Some(intent_state),
        }
    }
    
    /// 启动 Worker，处理 Intent
    pub async fn start(&mut self) -> Result<()> {
        info!("[PUSH WORKER] Started");
        
        while let Some(intent) = self.receiver.recv().await {
            if let Err(e) = self.process_intent(intent).await {
                error!("[PUSH WORKER] Failed to process intent: {}", e);
            }
        }
        
        Ok(())
    }
    
    async fn process_intent(&self, intent: PushIntent) -> Result<()> {
        info!(
            "[PUSH WORKER] Processing intent: intent_id={}, user_id={}, message_id={}",
            intent.intent_id, intent.user_id, intent.message_id
        );
        
        // Phase 3: 检查 Intent 状态（撤销/取消）
        if let Some(intent_state) = &self.intent_state {
            if let Some(status) = intent_state.get_status(&intent.intent_id).await {
                match status {
                    IntentStatus::Revoked => {
                        info!(
                            "[PUSH WORKER] Intent {} is revoked, skipping",
                            intent.intent_id
                        );
                        return Ok(());
                    }
                    IntentStatus::Cancelled => {
                        info!(
                            "[PUSH WORKER] Intent {} is cancelled, skipping",
                            intent.intent_id
                        );
                        return Ok(());
                    }
                    IntentStatus::Pending | IntentStatus::Processing => {
                        // 继续处理
                    }
                    IntentStatus::Sent => {
                        warn!(
                            "[PUSH WORKER] Intent {} already sent, skipping",
                            intent.intent_id
                        );
                        return Ok(());
                    }
                }
            }
        }
        
        // ✨ Phase 3.5: 如果 Intent 指定了 device_id，直接使用该设备
        if !intent.device_id.is_empty() {
            // 设备级 Intent：查询单个设备
            if let Some(repo) = &self.device_repo {
                match repo.get_device(intent.user_id, &intent.device_id).await {
                    Ok(Some(device)) => {
                        // 检查设备是否有 push_token
                        if device.push_token.is_none() {
                            debug!(
                                "[PUSH WORKER] Device {} has no push_token, skipping",
                                intent.device_id
                            );
                            return Ok(());
                        }
                        
                        // 生成 PushTask
                        let task = PushTask {
                            task_id: Uuid::new_v4().to_string(),
                            intent_id: intent.intent_id.clone(),
                            user_id: intent.user_id,
                            device_id: device.device_id.clone(),
                            vendor: device.vendor.clone(),
                            push_token: device.push_token.unwrap(),
                            payload: intent.payload.clone(),
                        };
                        
                        // 调用 Provider
                        return self.process_single_task(&task).await;
                    }
                    Ok(None) => {
                        debug!(
                            "[PUSH WORKER] Device {} not found, skipping",
                            intent.device_id
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        warn!(
                            "[PUSH WORKER] Failed to query device {}: {}",
                            intent.device_id, e
                        );
                        return Ok(());
                    }
                }
            } else {
                warn!("[PUSH WORKER] Device repository not configured, cannot process device-level intent");
                return Ok(());
            }
        }
        
        // 兼容旧逻辑：查询用户所有设备（如果 Intent 没有指定 device_id）
        let devices = if let Some(repo) = &self.device_repo {
            match repo.get_user_devices(intent.user_id).await {
                Ok(devices) => {
                    if devices.is_empty() {
                        debug!(
                            "[PUSH WORKER] User {} has no devices with push_token, skipping",
                            intent.user_id
                        );
                        return Ok(());
                    }
                    devices
                }
                Err(e) => {
                    warn!(
                        "[PUSH WORKER] Failed to query devices for user {}: {}, using mock",
                        intent.user_id, e
                    );
                    // 降级：使用 Mock Task
                    return self.process_mock_task(intent).await;
                }
            }
        } else {
            // 没有设备 Repository，使用 Mock
            debug!("[PUSH WORKER] Device repository not configured, using mock");
            return self.process_mock_task(intent).await;
        };
        
        // 2. 为每个设备生成 PushTask
        let mut success_count = 0;
        let mut failed_count = 0;
        
        for device in devices {
            let task = PushTask {
                task_id: Uuid::new_v4().to_string(),
                intent_id: intent.intent_id.clone(),
                user_id: intent.user_id,
                device_id: device.device_id.clone(),
                vendor: device.vendor.clone(),
                push_token: device.push_token.clone().unwrap_or_default(),
                payload: intent.payload.clone(),
            };
            
            // 3. 根据 vendor 选择 Provider
            let provider: Arc<dyn PushProvider> = match task.vendor {
                PushVendor::Fcm => {
                    // 如果配置了 FCM Provider，使用它；否则降级到 Mock
                    if let Some(ref fcm) = self.fcm_provider {
                        fcm.clone() as Arc<dyn PushProvider>
                    } else {
                        warn!(
                            "[PUSH WORKER] FCM Provider not configured, using mock for task {}",
                            task.task_id
                        );
                        self.mock_provider.clone() as Arc<dyn PushProvider>
                    }
                }
                PushVendor::Apns => {
                    // Phase 3: 如果配置了 APNs Provider，使用它；否则降级到 Mock
                    if let Some(ref apns) = self.apns_provider {
                        apns.clone() as Arc<dyn PushProvider>
                    } else {
                        warn!(
                            "[PUSH WORKER] APNs Provider not configured, using mock for task {}",
                            task.task_id
                        );
                        self.mock_provider.clone() as Arc<dyn PushProvider>
                    }
                }
            };
            
            match provider.send(&task).await {
                Ok(_) => {
                    success_count += 1;
                    debug!(
                        "[PUSH WORKER] Task {} sent successfully",
                        task.task_id
                    );
                }
                Err(e) => {
                    failed_count += 1;
                    error!(
                        "[PUSH WORKER] Failed to send task {}: {}",
                        task.task_id, e
                    );
                }
            }
        }
        
        info!(
            "[PUSH WORKER] Intent processed: intent_id={}, success={}, failed={}",
            intent.intent_id, success_count, failed_count
        );
        
        Ok(())
    }
    
    /// ✨ Phase 3.5: 处理单个 Task（设备级 Intent）
    async fn process_single_task(&self, task: &PushTask) -> Result<()> {
        // 根据 vendor 选择 Provider
        let provider: Arc<dyn PushProvider> = match task.vendor {
            PushVendor::Fcm => {
                if let Some(ref fcm) = self.fcm_provider {
                    fcm.clone() as Arc<dyn PushProvider>
                } else {
                    warn!(
                        "[PUSH WORKER] FCM Provider not configured, using mock for task {}",
                        task.task_id
                    );
                    self.mock_provider.clone() as Arc<dyn PushProvider>
                }
            }
            PushVendor::Apns => {
                if let Some(ref apns) = self.apns_provider {
                    apns.clone() as Arc<dyn PushProvider>
                } else {
                    warn!(
                        "[PUSH WORKER] APNs Provider not configured, using mock for task {}",
                        task.task_id
                    );
                    self.mock_provider.clone() as Arc<dyn PushProvider>
                }
            }
        };
        
        match provider.send(task).await {
            Ok(_) => {
                info!(
                    "[PUSH WORKER] Device-level task {} sent successfully",
                    task.task_id
                );
                Ok(())
            }
            Err(e) => {
                error!(
                    "[PUSH WORKER] Failed to send device-level task {}: {}",
                    task.task_id, e
                );
                Err(e)
            }
        }
    }
    
    /// 处理 Mock Task（降级方案）
    async fn process_mock_task(&self, intent: PushIntent) -> Result<()> {
        let task = PushTask {
            task_id: Uuid::new_v4().to_string(),
            intent_id: intent.intent_id.clone(),
            user_id: intent.user_id,
            device_id: "mock_device".to_string(),
            vendor: PushVendor::Apns,
            push_token: "mock_token".to_string(),
            payload: intent.payload,
        };
        
        self.mock_provider.send(&task).await?;
        Ok(())
    }
}
