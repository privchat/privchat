// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! QR_CODE_SPEC v1.3 — user/group 永久二维码模块。
//!
//! 与历史 `crate::rpc::qrcode` 模块的区别：
//!
//! | 维度       | `qrcode` (legacy)                                   | `qr` (本模块, v1.3)                                  |
//! |------------|------------------------------------------------------|------------------------------------------------------|
//! | 存储       | 进程内 `DashMap<qr_key, QRKeyRecord>`                | `privchat_users.qr_key` / `privchat_groups.qr_key` 字段 |
//! | 生命周期   | 短期 + 可过期 + revoke 标志                          | 永久跟随主表；用户/Owner 主动 `refresh` 旋转          |
//! | 用例       | 扫码登录 (Auth) / 临时活动二维码                     | 个人名片 / 群二维码                                  |
//! | URL 形态   | `privchat://<entity>/<action>?qrkey=...`             | `https://<host>/privchat:protocol/<entity>/<action>?qrkey=...` |
//! | RPC handler| 已存在的 `qrcode/*` + `user/qrcode/*` + `group/qrcode/*` | v1.3 handler 待 Step 2 接入                          |
//!
//! 本模块 Step 0+1 只提供两个原语：
//! - [`key`]: qr_key 生成器 + UNIQUE-conflict 检测
//! - [`url`]: 统一 URL builder + `qr_base_url` 规范化
//!
//! 后续 Step 2 在此模块下加 `user/`、`group/` 子目录承载具体 handler。

pub mod key;
pub mod url;

pub use key::{generate_qr_key, is_qr_key_unique_violation, QR_KEY_LEN, QR_KEY_MAX_GENERATION_RETRIES};
pub use url::{build_qr_url, normalize_qr_base_url, NormalizeQrBaseUrlError, QrEntity, QrAction};
