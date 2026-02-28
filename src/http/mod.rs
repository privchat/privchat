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

//! HTTP 服务器模块
//!
//! 包含两个独立的 HTTP 服务：
//! - 文件服务（对外）：文件上传、下载、URL 获取、删除
//! - 管理 API（仅内网）：用户管理、群组管理、统计等

pub mod dto;
pub mod middleware;
pub mod routes;
pub mod server;

pub use server::{AdminHttpServer, AdminServerState, FileHttpServer, FileServerState};
