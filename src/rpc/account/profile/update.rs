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

use crate::config::AccountMode;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 昵称上限。按**字符**算，不是字节：按字节算的话中文只能填三分之一。
pub const DISPLAY_NAME_MAX_CHARS: usize = 32;

/// 校验并归一化昵称。`Ok(None)` = 清空昵称。
pub fn normalize_display_name(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err(format!(
            "昵称最多 {} 个字符（当前 {}）",
            DISPLAY_NAME_MAX_CHARS,
            trimmed.chars().count()
        ));
    }
    // 控制字符会把一行昵称变成多行、或在列表里伪造空白，UI 层挡不干净。
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("昵称不能包含控制字符".to_string());
    }
    Ok(Some(trimmed.to_string()))
}

fn optional_str(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 本人修改自己的资料（昵称 / 头像）。
///
/// 🔴 只有 BUILTIN 部署能用。
///
/// PLATFORM 部署里账号体系在 privchat-application，IM 侧的 `display_name` 只是它
/// 异步镜像过来的副本。再开一个直写入口就有了两个写者：客户端这边刚写完，
/// 下一次镜像同步又把它盖回去，用户看到昵称自己变回去，而两边日志都显示"成功"。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    if services.config.account.mode != AccountMode::Builtin {
        return Err(RpcError::forbidden(
            "PLATFORM 模式下昵称与头像由平台账号系统管理，请调用平台资料接口",
        ));
    }

    // 身份来自会话，不从 body 读：body 里的 user_id 是客户端可控的。
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    // 没有 bio 这一列。悄悄丢掉调用方明确传进来的字段，会让它以为保存成功了。
    if body
        .get("bio")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(RpcError::validation("bio is not supported by this server"));
    }

    let display_name = match optional_str(&body, "display_name") {
        Some(raw) => Some(normalize_display_name(&raw).map_err(RpcError::validation)?),
        None => None,
    };
    let avatar_url = optional_str(&body, "avatar_url");
    if display_name.is_none() && avatar_url.is_none() {
        return Err(RpcError::validation(
            "nothing to update: display_name or avatar_url is required",
        ));
    }

    let user_service = crate::service::UserService::new(services.user_repository.clone());
    let updated = user_service
        .update_own_profile(
            user_id,
            // 外层 Option = 本次改不改；内层 = 改成什么（None 表示清空）。
            display_name.map(|name| name.unwrap_or_default()),
            avatar_url,
        )
        .await
        .map_err(|e| RpcError::internal(format!("更新个人资料失败: {}", e)))?;

    crate::service::invalidate_user_profile_everywhere(
        user_id,
        &services.cache_manager,
        &services.friend_service,
        &services.channel_service,
        services.connection_manager.clone(),
    )
    .await;

    Ok(json!({
        "status": "success",
        "user_id": updated.id.to_string(),
        "display_name": updated.display_name,
        "avatar_url": updated.avatar_url,
        "timestamp": chrono::Utc::now().timestamp_millis(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_and_accepts_a_normal_nickname() {
        assert_eq!(
            normalize_display_name("  张三  ").expect("valid"),
            Some("张三".to_string())
        );
    }

    /// 空串是"清空昵称"，不是错误——用户有权把昵称删掉。
    #[test]
    fn blank_means_clear() {
        assert_eq!(normalize_display_name("   ").expect("valid"), None);
    }

    /// 上限按字符算：32 个汉字要能存下。
    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let cjk = "字".repeat(DISPLAY_NAME_MAX_CHARS);
        assert!(normalize_display_name(&cjk).is_ok());
        assert!(normalize_display_name(&"字".repeat(DISPLAY_NAME_MAX_CHARS + 1)).is_err());
    }

    /// 换行会把一行昵称变成多行，列表布局当场错位。
    #[test]
    fn control_characters_are_rejected() {
        assert!(normalize_display_name("张\n三").is_err());
        assert!(normalize_display_name("张\u{0}三").is_err());
    }
}
