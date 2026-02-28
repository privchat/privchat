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

/// 安全模块
///
/// 提供 IM 服务器的核心安全能力：
/// - 四级状态机（Normal -> Throttled -> WriteDisabled -> ShadowBanned -> Disconnected）
/// - 多维度限流（user, user+rpc, user+channel, channel）
/// - Fan-out 成本计算
/// - IP 防护
/// - Shadow Ban
///
/// ## 分阶段启用策略
///
/// 通过 `SecurityMode` 控制能力启用程度：
/// - `ObserveOnly`: 只记录，不处罚（早期推荐）
/// - `EnforceLight`: 轻量限流
/// - `EnforceFull`: 全部特性
pub mod client_state;
pub mod rate_limiter;
pub mod security_service;

pub use client_state::{ClientState, ClientStateManager, TrustScore, ViolationType};
pub use rate_limiter::{
    FanoutCostCalculator, MultiDimensionRateLimiter, RateLimitConfig, RateLimitKey, RpcCost,
    RpcCostTable,
};
pub use security_service::{SecurityCheckResult, SecurityConfig, SecurityMode, SecurityService};
