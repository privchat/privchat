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

//! Web 扫码登录场景服务（spec QR_API §4）
//!
//! 状态机：created → scanned → authorized | rejected | expired
//!
//! 与 `QRCodeService`（名片/群邀请二维码）解耦：
//! - 此服务管理「Web 申请、App 扫码、状态机推进」的临时 scene
//! - 不持久化，DashMap 内存存储 + lazy expire（每次访问检查 `expires_at`）
//! - 默认 TTL：created 90s、scanned 60s、总寿命 ≤ 180s

use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::error::{Result, ServerError};
use crate::infra::ConnectionManager;
use crate::service::qr_login_publisher::QrLoginPublisher;
use privchat_protocol::rpc::qr_login::QrLoginPushEvent;

const DEFAULT_CREATED_TTL_SECS: i64 = 90;
const SCANNED_TTL_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QrSceneState {
    Created,
    Scanned,
    Authorized,
    Rejected,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebDeviceSnapshot {
    pub device_name: Option<String>,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QrScene {
    pub scene_id: String,
    pub qr_token: String,
    pub purpose: String,
    pub state: QrSceneState,
    pub web_device_id: String,
    pub web_device_info: WebDeviceSnapshot,
    pub created_at: i64,
    pub expires_at: i64,
    pub scanned_at: Option<i64>,
    pub scanner_uid: Option<u64>,
    pub scanner_device_id: Option<String>,
    pub scanner_avatar: Option<String>,
    pub scanner_display_name: Option<String>,
    pub confirm_token: Option<String>,
    pub confirm_token_consumed: bool,
}

impl QrScene {
    pub fn rpc_topic(&self) -> String {
        format!("qr_login.scene.{}", self.scene_id)
    }
}

#[derive(Debug)]
pub struct ScanResult {
    pub scene: QrScene,
    pub confirm_token: String,
}

/// Web 扫码登录服务（spec QR_API §4–§5）。
///
/// 状态机内存存储 + lazy expire；通过可选注入的 [`QrLoginPublisher`] 把 scan/reject/expire
/// transition 实时推回创建该 scene 的 unauth 连接（spec §5）。`authorized` 不在本 service
/// 推送 —— 那条事件需要 application 拼装好登录返回再显式 push（见 admin HTTP 路由）。
#[derive(Clone)]
pub struct QrLoginService {
    scenes: Arc<DashMap<String, QrScene>>,
    publisher: Option<Arc<QrLoginPublisher>>,
    connection_manager: Option<Arc<ConnectionManager>>,
}

impl Default for QrLoginService {
    fn default() -> Self {
        Self::new()
    }
}

impl QrLoginService {
    pub fn new() -> Self {
        Self {
            scenes: Arc::new(DashMap::new()),
            publisher: None,
            connection_manager: None,
        }
    }

    /// 注入 publisher + connection_manager，让状态转换自动推送事件给 unauth 连接。
    /// 没注入也能跑（HTTP 状态机仍正确），只是 Web 端拿不到实时事件。
    pub fn with_publisher(
        mut self,
        publisher: Arc<QrLoginPublisher>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        self.publisher = Some(publisher);
        self.connection_manager = Some(connection_manager);
        self
    }

    /// 创建场景（state=created）。
    pub fn create_scene(
        &self,
        purpose: String,
        web_device_id: String,
        web_device_info: WebDeviceSnapshot,
        ttl_secs: Option<i64>,
    ) -> QrScene {
        let scene_id = Uuid::new_v4().to_string();
        let qr_token = format!("qr_{}", Uuid::new_v4().simple());
        let now_ms = Utc::now().timestamp_millis();
        let ttl = ttl_secs.unwrap_or(DEFAULT_CREATED_TTL_SECS).max(1);
        let expires_at = now_ms + ttl * 1000;

        let scene = QrScene {
            scene_id: scene_id.clone(),
            qr_token,
            purpose,
            state: QrSceneState::Created,
            web_device_id,
            web_device_info,
            created_at: now_ms,
            expires_at,
            scanned_at: None,
            scanner_uid: None,
            scanner_device_id: None,
            scanner_avatar: None,
            scanner_display_name: None,
            confirm_token: None,
            confirm_token_consumed: false,
        };
        self.scenes.insert(scene_id, scene.clone());
        scene
    }

    /// 查询场景；过期 lazy 标记为 expired。
    pub fn get_scene(&self, scene_id: &str) -> Result<QrScene> {
        let mut entry = self
            .scenes
            .get_mut(scene_id)
            .ok_or_else(|| ServerError::NotFound(format!("QR_SCENE_NOT_FOUND: {}", scene_id)))?;
        Self::expire_if_due(&mut entry);
        Ok(entry.clone())
    }

    /// App 扫码：state=created → scanned；颁发 confirm_token。
    pub fn scan_scene(
        &self,
        scene_id: &str,
        scanner_uid: u64,
        scanner_device_id: String,
        qr_token: &str,
        scanner_avatar: Option<String>,
        scanner_display_name: Option<String>,
    ) -> Result<ScanResult> {
        let mut entry = self
            .scenes
            .get_mut(scene_id)
            .ok_or_else(|| ServerError::NotFound(format!("QR_SCENE_NOT_FOUND: {}", scene_id)))?;
        Self::expire_if_due(&mut entry);

        if entry.state != QrSceneState::Created {
            return Err(ServerError::Duplicate(format!(
                "QR_SCENE_STATE_INVALID: expected created, got {:?}",
                entry.state
            )));
        }
        if entry.qr_token != qr_token {
            return Err(ServerError::Validation(
                "QR_TOKEN_MISMATCH: qr_token 与场景不匹配".to_string(),
            ));
        }

        let now_ms = Utc::now().timestamp_millis();
        let confirm_token = format!("ct_{}", Uuid::new_v4().simple());

        entry.state = QrSceneState::Scanned;
        entry.scanned_at = Some(now_ms);
        entry.scanner_uid = Some(scanner_uid);
        entry.scanner_device_id = Some(scanner_device_id);
        entry.scanner_avatar = scanner_avatar;
        entry.scanner_display_name = scanner_display_name;
        entry.confirm_token = Some(confirm_token.clone());
        entry.confirm_token_consumed = false;
        // scanned 阶段重新计 expires_at（spec §3.3）
        entry.expires_at = now_ms + SCANNED_TTL_SECS * 1000;

        let scene_snapshot = entry.clone();
        let confirm_token_owned = confirm_token.clone();
        // 释放 DashMap 行锁后再异步 push，避免 Send + 死锁
        drop(entry);
        self.spawn_push(
            scene_snapshot.clone(),
            "qr_login.scanned",
            Some(json!({
                "scanner_uid": scene_snapshot.scanner_uid.unwrap_or_default(),
                "scanner_avatar": scene_snapshot.scanner_avatar,
                "scanner_display_name": scene_snapshot.scanner_display_name,
                "scanned_at": scene_snapshot.scanned_at.unwrap_or_default(),
            })),
            false,
        );

        Ok(ScanResult {
            scene: scene_snapshot,
            confirm_token: confirm_token_owned,
        })
    }

    /// App 确认：state=scanned → authorized；CAS 消费 confirm_token。
    pub fn confirm_scene(
        &self,
        scene_id: &str,
        scanner_uid: u64,
        scanner_device_id: &str,
        confirm_token: &str,
    ) -> Result<QrScene> {
        let mut entry = self
            .scenes
            .get_mut(scene_id)
            .ok_or_else(|| ServerError::NotFound(format!("QR_SCENE_NOT_FOUND: {}", scene_id)))?;
        Self::expire_if_due(&mut entry);

        Self::ensure_scanned_with_token(&entry, scanner_uid, scanner_device_id, confirm_token)?;
        entry.state = QrSceneState::Authorized;
        entry.confirm_token_consumed = true;
        Ok(entry.clone())
    }

    /// App 拒绝：state=scanned → rejected。
    pub fn reject_scene(
        &self,
        scene_id: &str,
        scanner_uid: u64,
        confirm_token: &str,
    ) -> Result<QrScene> {
        let mut entry = self
            .scenes
            .get_mut(scene_id)
            .ok_or_else(|| ServerError::NotFound(format!("QR_SCENE_NOT_FOUND: {}", scene_id)))?;
        Self::expire_if_due(&mut entry);

        if entry.state != QrSceneState::Scanned {
            return Err(ServerError::Duplicate(format!(
                "QR_SCENE_STATE_INVALID: expected scanned, got {:?}",
                entry.state
            )));
        }
        if entry.scanner_uid != Some(scanner_uid) {
            return Err(ServerError::PermissionDenied(
                "QR_SCANNER_MISMATCH: scanner_uid 与扫码者不一致".to_string(),
            ));
        }
        let valid_token = entry
            .confirm_token
            .as_deref()
            .map(|t| t == confirm_token && !entry.confirm_token_consumed)
            .unwrap_or(false);
        if !valid_token {
            return Err(ServerError::Validation(
                "QR_CONFIRM_TOKEN_INVALID: confirm_token 无效或已消费".to_string(),
            ));
        }

        entry.state = QrSceneState::Rejected;
        entry.confirm_token_consumed = true;
        let snapshot = entry.clone();
        drop(entry);
        self.spawn_push(snapshot.clone(), "qr_login.rejected", None, true);
        Ok(snapshot)
    }

    fn ensure_scanned_with_token(
        scene: &QrScene,
        scanner_uid: u64,
        scanner_device_id: &str,
        confirm_token: &str,
    ) -> Result<()> {
        if scene.state != QrSceneState::Scanned {
            return Err(ServerError::Duplicate(format!(
                "QR_SCENE_STATE_INVALID: expected scanned, got {:?}",
                scene.state
            )));
        }
        if scene.scanner_uid != Some(scanner_uid) {
            return Err(ServerError::PermissionDenied(
                "QR_SCANNER_MISMATCH: scanner_uid 与扫码者不一致".to_string(),
            ));
        }
        if scene.scanner_device_id.as_deref() != Some(scanner_device_id) {
            return Err(ServerError::PermissionDenied(
                "QR_SCANNER_MISMATCH: scanner_device_id 与扫码者不一致".to_string(),
            ));
        }
        let valid_token = scene
            .confirm_token
            .as_deref()
            .map(|t| t == confirm_token && !scene.confirm_token_consumed)
            .unwrap_or(false);
        if !valid_token {
            return Err(ServerError::Validation(
                "QR_CONFIRM_TOKEN_INVALID: confirm_token 无效或已消费".to_string(),
            ));
        }
        Ok(())
    }

    fn expire_if_due(scene: &mut QrScene) {
        if matches!(
            scene.state,
            QrSceneState::Authorized | QrSceneState::Rejected | QrSceneState::Expired
        ) {
            return;
        }
        let now = Utc::now().timestamp_millis();
        if now >= scene.expires_at {
            scene.state = QrSceneState::Expired;
        }
    }

    /// 标记 scene 为 authorized 并清掉 confirm_token，供 admin HTTP push 路径调用。
    /// 通常 confirm 已经把状态切为 authorized；这是 application 显式 push 之前的兜底，
    /// 用于 publisher 找不到状态时不至于推一个错误状态。
    pub fn mark_authorized(&self, scene_id: &str) -> Result<QrScene> {
        let mut entry = self
            .scenes
            .get_mut(scene_id)
            .ok_or_else(|| ServerError::NotFound(format!("QR_SCENE_NOT_FOUND: {}", scene_id)))?;
        entry.state = QrSceneState::Authorized;
        entry.confirm_token_consumed = true;
        Ok(entry.clone())
    }

    /// 主动扫一遍内存里的 scene，把已经过期但状态仍是 created/scanned 的转成 expired，
    /// 顺手把 `qr_login.expired` 推回 unauth 连接（spec §5）。
    /// 调用方应该周期性触发（如每 5s）。
    pub async fn tick_expired(&self) {
        let now = Utc::now().timestamp_millis();
        let mut to_publish: Vec<QrScene> = Vec::new();
        for mut entry in self.scenes.iter_mut() {
            if matches!(
                entry.state,
                QrSceneState::Authorized | QrSceneState::Rejected | QrSceneState::Expired
            ) {
                continue;
            }
            if now >= entry.expires_at {
                entry.state = QrSceneState::Expired;
                to_publish.push(entry.clone());
            }
        }
        for scene in to_publish {
            self.publish_now(scene, "qr_login.expired", None, true).await;
        }
    }

    fn spawn_push(
        &self,
        scene: QrScene,
        event: &'static str,
        data: Option<serde_json::Value>,
        terminal: bool,
    ) {
        let Some(publisher) = self.publisher.clone() else {
            return;
        };
        let Some(cm) = self.connection_manager.clone() else {
            return;
        };
        let push = QrLoginPushEvent {
            event: event.to_string(),
            scene_id: scene.scene_id.clone(),
            state: state_label(scene.state),
            data,
        };
        tokio::spawn(async move {
            let _ = publisher.push_event(&cm, push, terminal).await;
        });
    }

    async fn publish_now(
        &self,
        scene: QrScene,
        event: &'static str,
        data: Option<serde_json::Value>,
        terminal: bool,
    ) {
        let Some(publisher) = self.publisher.as_ref() else {
            return;
        };
        let Some(cm) = self.connection_manager.as_ref() else {
            return;
        };
        let push = QrLoginPushEvent {
            event: event.to_string(),
            scene_id: scene.scene_id.clone(),
            state: state_label(scene.state),
            data,
        };
        let _ = publisher.push_event(cm.as_ref(), push, terminal).await;
    }
}

fn state_label(state: QrSceneState) -> String {
    match state {
        QrSceneState::Created => "created",
        QrSceneState::Scanned => "scanned",
        QrSceneState::Authorized => "authorized",
        QrSceneState::Rejected => "rejected",
        QrSceneState::Expired => "expired",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_device() -> WebDeviceSnapshot {
        WebDeviceSnapshot {
            device_name: Some("Chrome".into()),
            user_agent: None,
            ip_address: Some("203.0.113.1".into()),
        }
    }

    #[test]
    fn create_then_scan_then_confirm() {
        let svc = QrLoginService::new();
        let scene = svc.create_scene("login".into(), "web-1".into(), make_device(), None);
        assert_eq!(scene.state, QrSceneState::Created);

        let r = svc
            .scan_scene(
                &scene.scene_id,
                100,
                "ios-1".into(),
                &scene.qr_token,
                None,
                Some("Alice".into()),
            )
            .unwrap();
        assert_eq!(r.scene.state, QrSceneState::Scanned);
        assert_eq!(r.scene.scanner_uid, Some(100));

        let confirmed = svc
            .confirm_scene(&scene.scene_id, 100, "ios-1", &r.confirm_token)
            .unwrap();
        assert_eq!(confirmed.state, QrSceneState::Authorized);
    }

    #[test]
    fn reject_after_scan() {
        let svc = QrLoginService::new();
        let scene = svc.create_scene("login".into(), "web-1".into(), make_device(), None);
        let r = svc
            .scan_scene(&scene.scene_id, 100, "ios-1".into(), &scene.qr_token, None, None)
            .unwrap();
        let rejected = svc
            .reject_scene(&scene.scene_id, 100, &r.confirm_token)
            .unwrap();
        assert_eq!(rejected.state, QrSceneState::Rejected);
    }

    #[test]
    fn scan_with_wrong_qr_token_rejected() {
        let svc = QrLoginService::new();
        let scene = svc.create_scene("login".into(), "web-1".into(), make_device(), None);
        let err = svc
            .scan_scene(&scene.scene_id, 100, "ios-1".into(), "wrong-token", None, None)
            .unwrap_err();
        assert!(format!("{}", err).contains("QR_TOKEN_MISMATCH"));
    }

    #[test]
    fn confirm_by_different_scanner_rejected() {
        let svc = QrLoginService::new();
        let scene = svc.create_scene("login".into(), "web-1".into(), make_device(), None);
        let r = svc
            .scan_scene(&scene.scene_id, 100, "ios-1".into(), &scene.qr_token, None, None)
            .unwrap();
        let err = svc
            .confirm_scene(&scene.scene_id, 200, "ios-1", &r.confirm_token)
            .unwrap_err();
        assert!(format!("{}", err).contains("QR_SCANNER_MISMATCH"));
    }

    #[test]
    fn confirm_token_single_use() {
        let svc = QrLoginService::new();
        let scene = svc.create_scene("login".into(), "web-1".into(), make_device(), None);
        let r = svc
            .scan_scene(&scene.scene_id, 100, "ios-1".into(), &scene.qr_token, None, None)
            .unwrap();
        svc.confirm_scene(&scene.scene_id, 100, "ios-1", &r.confirm_token)
            .unwrap();
        // 第二次：state 已 authorized，应 STATE_INVALID
        let err = svc
            .confirm_scene(&scene.scene_id, 100, "ios-1", &r.confirm_token)
            .unwrap_err();
        assert!(format!("{}", err).contains("QR_SCENE_STATE_INVALID"));
    }

    #[test]
    fn expired_after_ttl_marks_state() {
        let svc = QrLoginService::new();
        let scene = svc.create_scene("login".into(), "web-1".into(), make_device(), Some(1));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let s = svc.get_scene(&scene.scene_id).unwrap();
        assert_eq!(s.state, QrSceneState::Expired);
    }
}
