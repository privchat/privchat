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
