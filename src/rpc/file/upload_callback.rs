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

//! RPC: 上传完成回调（内部 RPC）
//!
//! 由文件服务器调用，通知业务服务器文件上传完成

use serde_json::{json, Value};
use tracing::warn;

use crate::rpc::{RpcError, RpcResult, RpcServiceContext};

/// 这次回调报的 `file_id` 能不能被接受。
///
/// 抽成纯函数是为了**能被测试驱动**：`RpcServiceContext` 有二十多个 Arc 服务，
/// 测试里构造不出来（本仓的 RPC 测试惯例也是抽纯判定，见 `rpc/group/group/info.rs`）。
///
/// 两道闸缺一不可：
/// 1. 墓碑说这次上传完成的就是这个 `file_id`——临时状态负责**定位**；
/// 2. 正式文件行与 token 冻结的身份一致——临时状态**不构成授权**。
///
/// 只有第 1 道的话，一个被篡改或串了的墓碑就能让回调对着别人的文件生效。
fn check_callback_target(
    tombstone: Option<u64>,
    reported: u64,
    meta: Option<&crate::service::file_service::FileMetadata>,
    token: &crate::service::upload_token_service::ValidatedUploadToken,
) -> Result<(), &'static str> {
    match tombstone {
        Some(expected) if expected == reported => {}
        Some(_) => return Err("file_id 与该次上传不符"),
        None => return Err("该次上传尚未完成，无法回调"),
    }
    let meta = meta.ok_or("file_id 不存在")?;
    if !token.matches_file(meta) {
        return Err("file_id 与本次上传的身份不符");
    }
    Ok(())
}

/// 上传完成回调
pub async fn upload_callback(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    // 解析参数
    let upload_token = params
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("缺少 token 参数".to_string()))?;

    let file_id = params["file_id"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 file_id 参数".to_string()))?
        .to_string();

    let _file_url = params["file_url"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 file_url 参数".to_string()))?
        .to_string();

    let _thumbnail_url = params["thumbnail_url"].as_str().map(|s| s.to_string());

    let file_size = params["file_size"]
        .as_u64()
        .ok_or_else(|| RpcError::validation("缺少 file_size 参数".to_string()))?;

    let _original_size = params["original_size"].as_u64();

    let _mime_type = params["mime_type"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 mime_type 参数".to_string()))?
        .to_string();

    let width = params["width"].as_u64().map(|v| v as u32);
    let height = params["height"].as_u64().map(|v| v as u32);

    tracing::debug!(
        "📤 文件上传完成回调: file_id={}, size={} bytes",
        file_id,
        file_size
    );

    // 🔴 **token 仍然有效是正常的，不是异常** —— 但无效必须拒绝。
    //
    // 这里原本把「token 还能验过」当告警、随后 `remove_token`，而验证失败只记一条
    // warning 就照样返回成功。两条都不对：前者的语义建立在「一次性 5 分钟 token」上
    //（现在 token 最长 24 小时、可复用，签名 token 服务端根本不存储，没有东西可删）；
    // 后者等于**任何无效 token 都能拿到成功回调**。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let token_info = services
        .upload_token_service
        .validate_any(now_secs, upload_token)
        .await
        .map_err(|e| {
            warn!(
                "❌ 上传回调 token 无效: {}… ({e})",
                upload_token.chars().take(8).collect::<String>()
            );
            RpcError::validation("上传 token 无效".to_string())
        })?;

    // claim 用途的 token 换不出「我传完了」这件事。
    if token_info.purpose
        != crate::service::upload_token_service::UploadTokenPurpose::Upload
    {
        return Err(RpcError::validation(
            "该 token 用于秒传取用，不能用作上传完成回调".to_string(),
        ));
    }

    // 🔴 `file_id` 必须**确实是这次上传的结果**，不能由调用方随口报一个。
    //
    // 判据取自**临时会话**（`state.json` 的墓碑），不查业务库：上传中间态不进
    // PostgreSQL。会话已被清理或丢失时，这次回调就没有可核对的依据，直接拒绝——
    // 客户端重新申请 token 从头传即可。
    let file_id_num: u64 = file_id
        .parse()
        .map_err(|_| RpcError::validation("file_id 不是合法数字".to_string()))?;
    let session_root = services
        .file_service
        .upload_session_root()
        .map_err(|e| RpcError::internal(e.to_string()))?;
    // 🔴 `open_existing`：恢复类入口不得惰性建目录。会话没了就是没了
    //（`SessionGone` 语义），不该被伪装成「有一个空会话」，也不该留下垃圾目录。
    let session = crate::service::upload_session::UploadSession::open_existing(
        &session_root,
        token_info.user_id,
        &token_info.upload_id,
    )
    .map_err(|e| RpcError::internal(e.to_string()))?
    .ok_or_else(|| RpcError::validation("该次上传的会话已不存在".to_string()))?;
    let tombstone = session
        .completed_file_id()
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let meta = services
        .file_service
        .get_file_metadata(file_id_num)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    if let Err(reason) = check_callback_target(tombstone, file_id_num, meta.as_ref(), &token_info) {
        warn!("❌ 上传回调被拒: {reason}（file_id={file_id_num}）");
        return Err(RpcError::validation(reason.to_string()));
    }

    // TODO: 记录文件元数据到数据库
    // TODO: 更新用户配额
    // TODO: 触发后续业务逻辑（如媒体处理、内容审核）

    Ok(json!({
        "success": true,
        "message": "文件上传成功",
    }))
}

#[cfg(test)]
mod tests {
    use super::check_callback_target;
    use crate::model::file_upload::FileType;
    use crate::service::file_service::FileMetadata;
    use crate::service::upload_token_service::{
        UploadIdentity, UploadToken, UploadTokenPurpose, ValidatedUploadToken,
    };

    fn token() -> ValidatedUploadToken {
        let record = UploadToken::new(
            42,
            FileType::Image,
            10 * 1024 * 1024,
            "message".to_string(),
            None,
            UploadIdentity {
                sha256: Some("a".repeat(64)),
                declared_size: Some(4096),
                mime_type: Some("image/png".to_string()),
                transform_version: 0,
            },
            UploadTokenPurpose::Upload,
            600,
        );
        ValidatedUploadToken::from_legacy("b3f1a2c4-0000-4000-8000-000000000001", &record)
    }

    fn meta() -> FileMetadata {
        FileMetadata {
            file_id: 900,
            original_filename: "holiday.png".to_string(),
            file_size: 4096,
            original_size: None,
            file_type: FileType::Image,
            mime_type: "image/png".to_string(),
            file_path: "images/900.png".to_string(),
            storage_source_id: 0,
            uploader_id: 42,
            uploader_ip: None,
            uploaded_at: 0,
            width: None,
            height: None,
            file_hash: Some("a".repeat(64)),
            business_type: Some("message".to_string()),
            business_id: None,
            encryption_version: 0,
            cek: None,
        }
    }

    #[test]
    fn a_matching_callback_is_accepted() {
        assert!(check_callback_target(Some(900), 900, Some(&meta()), &token()).is_ok());
    }

    /// 🔴 这条是 callback 身份门禁的直接守卫：**墓碑对得上，正式文件却不是这一份**。
    ///
    /// 只比墓碑的话，一个被篡改或串了的墓碑就能让回调对着别人的文件生效。
    /// 删掉 `check_callback_target` 里的 `matches_file`，下面三种都会漏过去。
    #[test]
    fn a_callback_whose_file_does_not_match_the_token_is_refused() {
        let t = token();

        let mut wrong_digest = meta();
        wrong_digest.file_hash = Some("b".repeat(64));
        assert!(check_callback_target(Some(900), 900, Some(&wrong_digest), &t).is_err());

        let mut wrong_size = meta();
        wrong_size.file_size = 8192;
        assert!(check_callback_target(Some(900), 900, Some(&wrong_size), &t).is_err());

        let mut wrong_type = meta();
        wrong_type.file_type = FileType::File;
        assert!(check_callback_target(Some(900), 900, Some(&wrong_type), &t).is_err());

        let mut other_owner = meta();
        other_owner.uploader_id = 43;
        assert!(check_callback_target(Some(900), 900, Some(&other_owner), &t).is_err());
    }

    /// 报的 file_id 与墓碑不符：调用方不能随口指定别的文件。
    #[test]
    fn a_callback_reporting_another_file_id_is_refused() {
        assert!(check_callback_target(Some(900), 901, Some(&meta()), &token()).is_err());
    }

    /// 会话还没完成（或墓碑已被清理）时不该有回调。
    #[test]
    fn a_callback_without_a_completed_session_is_refused() {
        assert!(check_callback_target(None, 900, Some(&meta()), &token()).is_err());
    }

    /// 墓碑指向一条读不到的记录：拒绝，而不是当成成功。
    #[test]
    fn a_callback_pointing_at_a_missing_row_is_refused() {
        assert!(check_callback_target(Some(900), 900, None, &token()).is_err());
    }
}
