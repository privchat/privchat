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

//! 中间件模块
//!
//! 提供各种请求处理中间件，包括：
//! - 认证中间件（AuthMiddleware）
//! - 安全中间件（SecurityMiddleware）

pub mod auth_middleware;
pub mod security_middleware;

pub use auth_middleware::AuthMiddleware;
pub use security_middleware::SecurityMiddleware;
