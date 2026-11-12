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

use crate::rpc::error::{RpcError, RpcResult};
use serde_json::Value;

/// `account/user/update` 已废弃：不作为资料写入入口。
///
/// MODULE_MEMBER_PROFILE_SPEC §11 写死「privchat-server 不增加任何用户面 RPC 处理
/// profile」。本人改昵称/头像的唯一 IM RPC 入口是 `account/profile/update`（仅
/// BUILTIN 部署；PLATFORM 部署由平台账号系统管理，走平台资料接口）。
///
/// 这条路由历史上是个返回 success 却什么都不写的桩：调用方拿到假成功、资料纹丝
/// 不动。现在显式拒绝并指向权威路径，绝不静默成功。拒绝是扁平策略，不读 body /
/// 会话 / 服务，因此签名只保留路由器要求的 (body, ctx)。
pub async fn handle(_body: Value, _ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    Err(deprecated_profile_write_entry_error())
}

fn deprecated_profile_write_entry_error() -> RpcError {
    RpcError::forbidden(
        "account/user/update 已废弃，不作为资料写入入口（MODULE_MEMBER_PROFILE_SPEC §11）。\
         本人改昵称/头像请用 account/profile/update（BUILTIN 部署）；\
         PLATFORM 部署请走平台资料接口。",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use privchat_protocol::ErrorCode;

    /// 废弃路由必须拒绝，不能返回假成功；拒绝须指向权威写入路径。
    /// 直接打真实 handler（无需 DB / 服务上下文）， reverted 成 Ok(success) 会红。
    #[tokio::test]
    async fn rejects_instead_of_faking_success() {
        let err = handle(
            serde_json::json!({ "display_name": "张三" }),
            crate::rpc::RpcContext::new(),
        )
        .await
        .expect_err("account/user/update 必须拒绝，不能静默成功");
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert!(
            err.message.contains("account/profile/update"),
            "拒绝须指向权威写入路径，实际: {}",
            err.message
        );
    }
}
