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

use crate::error::Result;
use crate::push::intent_state::IntentStateManager;
use crate::push::provider::{
    ApnsProvider, FcmProvider, HmsProvider, LenovoProvider, MeizuProvider, MockProvider,
    OppoProvider, PushProvider, VivoProvider, XiaomiProvider, ZteProvider,
};
use crate::push::types::{IntentStatus, PushIntent, PushTask, PushVendor};
use crate::repository::UserDeviceRepository;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

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
    fcm_provider: Option<Arc<FcmProvider>>, // Phase 2: FCM Provider（可选）
    apns_provider: Option<Arc<ApnsProvider>>, // Phase 3: APNs Provider（可选）
    hms_provider: Option<Arc<HmsProvider>>, // HMS Provider（可选）
    honor_provider: Option<Arc<HmsProvider>>, // Honor 复用 HMS 协议
    xiaomi_provider: Option<Arc<XiaomiProvider>>,
    oppo_provider: Option<Arc<OppoProvider>>,
    vivo_provider: Option<Arc<VivoProvider>>,
    lenovo_provider: Option<Arc<LenovoProvider>>,
    zte_provider: Option<Arc<ZteProvider>>,
    meizu_provider: Option<Arc<MeizuProvider>>,
    device_repo: Option<Arc<UserDeviceRepository>>,
    intent_state: Option<Arc<IntentStateManager>>, // Phase 3: Intent 状态管理器
}

impl PushWorker {
    pub fn new(receiver: mpsc::Receiver<PushIntent>) -> Self {
        Self {
            receiver,
            mock_provider: Arc::new(MockProvider),
            fcm_provider: None,
            apns_provider: None,
            hms_provider: None,
            honor_provider: None,
            xiaomi_provider: None,
            oppo_provider: None,
            vivo_provider: None,
            lenovo_provider: None,
            zte_provider: None,
            meizu_provider: None,
            device_repo: None,
            intent_state: None,
        }
    }

    /// 创建带设备 Repository 的 Worker（Phase 2）
    pub fn with_device_repo(
        receiver: mpsc::Receiver<PushIntent>,
        device_repo: Arc<UserDeviceRepository>,
    ) -> Self {
        Self {
            receiver,
            mock_provider: Arc::new(MockProvider),
            fcm_provider: None,
            apns_provider: None,
            hms_provider: None,
            honor_provider: None,
            xiaomi_provider: None,
            oppo_provider: None,
            vivo_provider: None,
            lenovo_provider: None,
            zte_provider: None,
            meizu_provider: None,
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
        hms_provider: Option<Arc<HmsProvider>>,
        honor_provider: Option<Arc<HmsProvider>>,
        xiaomi_provider: Option<Arc<XiaomiProvider>>,
        oppo_provider: Option<Arc<OppoProvider>>,
        vivo_provider: Option<Arc<VivoProvider>>,
        lenovo_provider: Option<Arc<LenovoProvider>>,
        zte_provider: Option<Arc<ZteProvider>>,
        meizu_provider: Option<Arc<MeizuProvider>>,
    ) -> Self {
        Self {
            receiver,
            mock_provider: Arc::new(MockProvider),
            fcm_provider,
            apns_provider,
            hms_provider,
            honor_provider,
            xiaomi_provider,
            oppo_provider,
            vivo_provider,
            lenovo_provider,
            zte_provider,
            meizu_provider,
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
                        // 设备级 intent 走 get_device，那条查询不带 apns_armed 过滤
                        // （它还要服务于"这台设备当前什么状态"的读取），所以在这里判。
                        if !device.apns_armed {
                            debug!(
                                "[PUSH WORKER] Device {} 未开启推送（apns_armed=false），跳过",
                                intent.device_id
                            );
                            return Ok(());
                        }
                        let Some(push_token) =
                            device.push_token.filter(|it| !it.trim().is_empty())
                        else {
                            debug!(
                                "[PUSH WORKER] Device {} 没有 push_token，跳过",
                                intent.device_id
                            );
                            return Ok(());
                        };
                        let task = PushTask {
                            task_id: Uuid::new_v4().to_string(),
                            intent_id: intent.intent_id.clone(),
                            user_id: intent.user_id,
                            device_id: device.device_id.clone(),
                            vendor: device.vendor.clone(),
                            push_token,
                            locale: device.locale.clone(),
                            push_sound: device.push_sound,
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
                // 空 token 发出去只会换来 provider 的 BadDeviceToken，白白占一次配额。
                push_token: match device.push_token.clone().filter(|it| !it.trim().is_empty()) {
                    Some(token) => token,
                    None => continue,
                },
                locale: device.locale.clone(),
                push_sound: device.push_sound,
                payload: intent.payload.clone(),
            };

            // 3. 根据 vendor 选择 Provider
            let Some(provider) = self.resolve_provider(&task.vendor) else {
                error!(
                    "[PUSH WORKER] {:?} provider 未配置，跳过 device={}（intent {}）",
                    task.vendor, task.device_id, intent.intent_id
                );
                failed_count += 1;
                continue;
            };

            match provider.send(&task).await {
                Ok(_) => {
                    success_count += 1;
                    debug!("[PUSH WORKER] Task {} sent successfully", task.task_id);
                    // [TRACE] Node 3: push_sent
                    {
                        use crate::infra::delivery_trace::{global_trace_store, stages};
                        global_trace_store()
                            .record(
                                intent.message_id,
                                stages::PUSH_SENT,
                                format!("device={}", task.device_id),
                            )
                            .await;
                    }
                }
                Err(e) => {
                    failed_count += 1;
                    self.handle_invalid_token(&e, &task).await;
                    error!("[PUSH WORKER] Failed to send task {}: {}", task.task_id, e);
                    // [TRACE] Node 4: push_failed
                    {
                        use crate::infra::delivery_trace::{global_trace_store, stages};
                        global_trace_store()
                            .record(
                                intent.message_id,
                                stages::PUSH_FAILED,
                                format!("device={} err={}", task.device_id, e),
                            )
                            .await;
                    }
                }
            }
        }

        info!(
            "[PUSH WORKER] Intent processed: intent_id={}, success={}, failed={}",
            intent.intent_id, success_count, failed_count
        );

        Ok(())
    }



    /// provider 说这个 token 已经死了 → 从库里清掉，别再对它重试。
    ///
    /// 只对 `PushTokenInvalid` 动手：网络抖动、限流、5xx 都不该导致用户丢掉推送能力。
    async fn handle_invalid_token(&self, error: &crate::error::ServerError, task: &PushTask) {
        if !matches!(error, crate::error::ServerError::PushTokenInvalid(_)) {
            return;
        }
        let Some(repo) = &self.device_repo else { return };
        if let Err(e) = repo
            .invalidate_push_token(task.user_id, &task.device_id, &task.push_token)
            .await
        {
            warn!("[PUSH WORKER] 清理失效 push token 失败: {}", e);
        }
    }

    /// vendor → provider。**没配就是没配**：返回 None，由调用方计入失败并留日志。
    ///
    /// 这里以前会在 provider 缺失时退回 MockProvider，于是 push_sent 照常打印、
    /// trace 照常记成功，而手机上什么都没有——排查时先看到的是一串"发送成功"。
    /// 假成功比不发更贵。
    fn resolve_provider(&self, vendor: &PushVendor) -> Option<Arc<dyn PushProvider>> {
        match vendor {
            PushVendor::Fcm => self.fcm_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Apns => self.apns_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Hms => self.hms_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            // Honor 复用 HMS 协议：优先用独立凭证，没有就退回 HMS 凭证。
            PushVendor::Honor => self
                .honor_provider
                .clone()
                .or_else(|| self.hms_provider.clone())
                .map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Xiaomi => self.xiaomi_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Oppo => self.oppo_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Vivo => self.vivo_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Lenovo => self.lenovo_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Zte => self.zte_provider.clone().map(|p| p as Arc<dyn PushProvider>),
            PushVendor::Meizu => self.meizu_provider.clone().map(|p| p as Arc<dyn PushProvider>),
        }
    }

    /// ✨ Phase 3.5: 处理单个 Task（设备级 Intent）
    async fn process_single_task(&self, task: &PushTask) -> Result<()> {
        // 根据 vendor 选择 Provider
        let Some(provider) = self.resolve_provider(&task.vendor) else {
            error!(
                "[PUSH WORKER] {:?} provider 未配置，task {} 未发送（device={}）",
                task.vendor, task.task_id, task.device_id
            );
            return Err(crate::error::ServerError::Internal(format!(
                "push provider not configured for vendor {:?}",
                task.vendor
            )));
        };

        match provider.send(task).await {
            Ok(_) => {
                info!(
                    "[PUSH WORKER] Device-level task {} sent successfully",
                    task.task_id
                );
                // [TRACE] Node 3: push_sent (device-level)
                {
                    use crate::infra::delivery_trace::{global_trace_store, stages};
                    global_trace_store()
                        .record(
                            task.payload.message_id,
                            stages::PUSH_SENT,
                            format!("device={}", task.device_id),
                        )
                        .await;
                }
                Ok(())
            }
            Err(e) => {
                error!(
                    "[PUSH WORKER] Failed to send device-level task {}: {}",
                    task.task_id, e
                );
                self.handle_invalid_token(&e, task).await;
                // [TRACE] Node 4: push_failed (device-level)
                {
                    use crate::infra::delivery_trace::{global_trace_store, stages};
                    global_trace_store()
                        .record(
                            task.payload.message_id,
                            stages::PUSH_FAILED,
                            format!("device={} err={}", task.device_id, e),
                        )
                        .await;
                }
                Err(e)
            }
        }
    }

    /// 设备仓库不可用时的处理。
    ///
    /// 以前这里会伪造一条 `mock_device` 任务发给 MockProvider 并返回成功——数据库抖一下，
    /// 日志就多一条"推送成功"，而那条消息谁也没收到。查不到设备就是发不出去，如实报错。
    async fn process_mock_task(&self, intent: PushIntent) -> Result<()> {
        error!(
            "[PUSH WORKER] 设备仓库不可用，intent {} (user={}) 未推送",
            intent.intent_id, intent.user_id
        );
        Err(crate::error::ServerError::Internal(
            "device repository unavailable, push not sent".to_string(),
        ))
    }
}
