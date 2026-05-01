// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Licensed under the Apache License, Version 2.0.

//! QR Login HTTP DTOs（spec QR_API §4）。

use serde::{Deserialize, Serialize};

use crate::service::qr_login_service::{QrSceneState, WebDeviceSnapshot};

// ────────── 4.1 Create ──────────

#[derive(Debug, Deserialize)]
pub struct CreateQrSceneRequest {
    /// `login` —— v1.2 仅支持登录。
    #[serde(default = "default_purpose")]
    pub purpose: String,
    /// Web 端持久化 device_id（IDENTITY §7.4）。
    pub device_id: String,
    pub device_info: WebDeviceInfoInput,
    /// `created → scanned` 阶段过期秒数；缺省 90s。
    pub ttl: Option<i64>,
}

fn default_purpose() -> String {
    "login".to_string()
}

#[derive(Debug, Deserialize)]
pub struct WebDeviceInfoInput {
    pub app_id: Option<String>,
    pub device_name: Option<String>,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
}

impl WebDeviceInfoInput {
    pub fn into_snapshot(self) -> WebDeviceSnapshot {
        WebDeviceSnapshot {
            device_name: self.device_name,
            user_agent: self.user_agent,
            ip_address: self.ip_address,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct QrSceneResponse {
    pub scene_id: String,
    pub qr_token: String,
    pub expires_at: i64,
    pub rpc_topic: String,
}

// ────────── 4.2 Get ──────────

#[derive(Debug, Serialize)]
pub struct QrSceneStatusResponse {
    pub scene_id: String,
    pub state: QrSceneState,
    pub expires_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanner_uid: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanner_avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanner_display_name: Option<String>,
}

// ────────── 4.3 Scan ──────────

#[derive(Debug, Deserialize)]
pub struct ScanQrSceneRequest {
    pub scanner_uid: u64,
    pub scanner_device_id: String,
    pub qr_token: String,
    pub scanner_avatar: Option<String>,
    pub scanner_display_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScanQrSceneResponse {
    pub scene_id: String,
    pub state: QrSceneState,
    pub confirm_token: String,
    pub purpose: String,
    pub web_device_info: WebDeviceSnapshot,
}

// ────────── 4.4 Confirm ──────────

#[derive(Debug, Deserialize)]
pub struct ConfirmQrSceneRequest {
    pub scanner_uid: u64,
    pub scanner_device_id: String,
    pub confirm_token: String,
}

#[derive(Debug, Serialize)]
pub struct ConfirmQrSceneResponse {
    pub scene_id: String,
    pub state: QrSceneState,
    pub uid: u64,
    /// 创建 scene 时由 Web 端提供的 device_id；application 用它给 Web 签发登录 token。
    pub web_device_id: String,
    /// Web 设备快照（spec QR_API §5）；application 用它构造 issueImToken 的 DeviceInfo。
    pub web_device_info: WebDeviceSnapshot,
}

// ────────── 4.5 Reject ──────────

#[derive(Debug, Deserialize)]
pub struct RejectQrSceneRequest {
    pub scanner_uid: u64,
    pub confirm_token: String,
}

#[derive(Debug, Serialize)]
pub struct RejectQrSceneResponse {
    pub scene_id: String,
    pub state: QrSceneState,
}

// ────────── 5.x Push Authorized（spec QR_API §5） ──────────

/// 由 application 调用：把 application 端组装好的登录返回对象（透明 JSON）
/// 通过 server 的 unauth publisher 推回给 Web 端。
///
/// `data` 直接是 `MemberLoginResponse` 的 JSON 形态；server 不解析 schema，避免
/// 协议层和 application 强耦合（spec QR_API §5）。
#[derive(Debug, Deserialize)]
pub struct PushQrAuthorizedRequest {
    pub data: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct PushQrAuthorizedResponse {
    pub scene_id: String,
    pub delivered: bool,
}
