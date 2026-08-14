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

/// 回调失败的三类。**分类的依据是「客户端接下来该做什么」，不是错误发生在哪一层。**
///
/// 🔴 只分两类不够，而且两次都错在同一个地方：
///   · 一开始全压成静态字符串 → 数据库抖动被标成参数错误，客户端**不再重试**；
///   · 补成 Rejected/Internal 后 → 会话丢失被塞进这两类，一边让客户端永久放弃、
///     一边让它对着一个**永远不会自愈**的状态反复重试。
///
/// 会话丢失是既定协议里的第三种结局：**重来一遍**（重新 prepare 拿 token 再传），
/// 它既不是客户端参数错，也不是重试能解决的抖动。
#[derive(Debug)]
pub(crate) enum CallbackError {
    /// 参数/身份不对 → `RpcError::validation`（`InvalidParams`）。**重试无用，别重试。**
    Rejected(&'static str),
    /// 临时会话没了或已损坏 → `RpcError::not_found`（`ResourceNotFound`）。
    /// **重试同一个调用永远不会好**，客户端应清掉本地会话、重新 prepare 从头传。
    SessionGone(&'static str),
    /// 基础设施抖动：数据库、网络、磁盘 I/O → `RpcError::internal`。**可以重试。**
    Internal(String),
}

impl CallbackError {
    pub(crate) fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }

    pub(crate) fn is_session_gone(&self) -> bool {
        matches!(self, Self::SessionGone(_))
    }

    pub(crate) fn is_internal(&self) -> bool {
        matches!(self, Self::Internal(_))
    }
}

/// 回调的**完整编排**：开会话（不惰性创建）→ 读墓碑 → 回读正式行 → 核对身份。
///
/// 🔴 依赖收窄到「一个目录 + 一个按 id 取记录的闭包」，就是为了让**这条链路本身**
/// 能被测试驱动，而不是只测最里层的判据。
/// `RpcServiceContext` 有二十多个 Arc 服务、测试里构造不出来——那是缩小可测边界的
/// 理由，不是不测的理由。RPC 于是退化成薄适配：解析参数、把服务接进来。
///
/// 两道闸缺一不可：
/// 1. 墓碑说这次上传完成的就是这个 `file_id`——临时状态负责**定位**；
/// 2. 正式文件行与 token 冻结的身份一致——临时状态**不构成授权**。
///
/// 只有第 1 道的话，一个被篡改或串了的墓碑就能让回调对着别人的文件生效。
pub(crate) async fn authorise_callback<F, Fut>(
    session_root: &std::path::Path,
    token: &crate::service::upload_token_service::ValidatedUploadToken,
    reported: u64,
    fetch_meta: F,
) -> Result<crate::service::file_service::FileMetadata, CallbackError>
where
    F: FnOnce(u64) -> Fut,
    Fut: std::future::Future<
        Output = Result<Option<crate::service::file_service::FileMetadata>, String>,
    >,
{
    // 🔴 `open_existing`：恢复类入口不得惰性建目录。会话没了就是没了
    //（`SessionGone` 语义），不该被伪装成「有一个空会话」，也不该留下垃圾目录。
    let session = crate::service::upload_session::UploadSession::open_existing(
        session_root,
        token.user_id,
        &token.upload_id,
    )
    .map_err(|e| CallbackError::Internal(format!("会话不可读: {e}")))?
    .ok_or(CallbackError::SessionGone("该次上传的会话已不存在"))?;

    // 🔴 状态文件损坏是**持久**故障：重复回调不会让它变好，重试只是空转。
    // 归到 SessionGone，让客户端重新 prepare。
    let tombstone = session
        .completed_file_id()
        .map_err(|_| CallbackError::SessionGone("会话状态已损坏"))?;
    check_callback_target_id(tombstone, reported).map_err(CallbackError::Rejected)?;

    let meta = fetch_meta(reported)
        .await
        .map_err(|e| CallbackError::Internal(format!("读取文件记录失败: {e}")))?
        .ok_or(CallbackError::Rejected("file_id 不存在"))?;
    if !token.matches_file(&meta) {
        return Err(CallbackError::Rejected("file_id 与本次上传的身份不符"));
    }
    Ok(meta)
}

/// 墓碑这一道闸：报的 `file_id` 必须就是这次上传完成的那个。
fn check_callback_target_id(tombstone: Option<u64>, reported: u64) -> Result<(), &'static str> {
    match tombstone {
        Some(expected) if expected == reported => Ok(()),
        Some(_) => Err("file_id 与该次上传不符"),
        None => Err("该次上传尚未完成，无法回调"),
    }
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
    // 判据取自**临时会话**（墓碑）+ 正式行身份，全部在 `authorise_callback` 里，
    // 这里只负责把服务接进去。
    let file_id_num: u64 = file_id
        .parse()
        .map_err(|_| RpcError::validation("file_id 不是合法数字".to_string()))?;
    let session_root = services
        .file_service
        .upload_session_root()
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let file_service = services.file_service.clone();
    authorise_callback(&session_root, &token_info, file_id_num, |id| async move {
        file_service
            .get_file_metadata(id)
            .await
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| match e {
        CallbackError::Rejected(reason) => {
            warn!("❌ 上传回调被拒: {reason}（file_id={file_id_num}）");
            RpcError::validation(reason.to_string())
        }
        // 🔴 基础设施故障必须是 internal：标成 validation 等于告诉客户端
        // 「别重试了」，一次数据库抖动就把这次回调永久判死。
        // 会话没了 / 坏了：给一个客户端认得出的码，让它重新 prepare，
        // 而不是永久放弃，也不是对着不会自愈的状态死循环。
        CallbackError::SessionGone(reason) => {
            warn!("🔁 上传会话不可恢复: {reason}（file_id={file_id_num}），需重新 prepare");
            RpcError::not_found(reason.to_string())
        }
        // 🔴 基础设施故障必须是 internal：标成 validation 等于告诉客户端
        // 「别重试了」，一次数据库抖动就把这次回调永久判死。
        CallbackError::Internal(detail) => {
            warn!("💥 上传回调处理失败（可重试）: {detail}（file_id={file_id_num}）");
            RpcError::internal(detail)
        }
    })?;

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
    use super::{authorise_callback, check_callback_target_id};
    use crate::model::file_upload::FileType;
    use crate::service::file_service::FileMetadata;
    use crate::service::upload_session::UploadSession;
    use crate::service::upload_token_service::{
        UploadIdentity, UploadToken, UploadTokenPurpose, ValidatedUploadToken,
    };

    const UPLOAD_ID: &str = "b3f1a2c40000400080000000000000ff";

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
        let mut v = ValidatedUploadToken::from_legacy("irrelevant-raw-token", &record);
        // 直接指定 upload_id，好让测试自己布置会话目录。
        v.upload_id = UPLOAD_ID.to_string();
        v
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

    /// 布置一个「已完成」的会话。
    fn completed_session(root: &std::path::Path, file_id: u64) {
        let s = UploadSession::open(root, 42, UPLOAD_ID).expect("open");
        s.mark_completed(file_id).expect("mark");
    }

    async fn run(
        root: &std::path::Path,
        reported: u64,
        found: Option<FileMetadata>,
    ) -> Result<FileMetadata, super::CallbackError> {
        authorise_callback(root, &token(), reported, |_| async move { Ok(found) }).await
    }

    #[tokio::test]
    async fn a_matching_callback_is_accepted() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        assert!(run(r.path(), 900, Some(meta())).await.is_ok());
    }

    /// 🔴 **这条守的是整条链路**：墓碑对得上，正式文件却不是这一份。
    ///
    /// 只比墓碑的话，一个被篡改或串了的墓碑就能让回调对着别人的文件生效。
    /// 把 `authorise_callback` 里的 `matches_file` 去掉，这四种都会漏过去。
    #[tokio::test]
    async fn a_callback_whose_file_does_not_match_the_token_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);

        let mut wrong_digest = meta();
        wrong_digest.file_hash = Some("b".repeat(64));
        assert!(run(r.path(), 900, Some(wrong_digest)).await.is_err());

        let mut wrong_size = meta();
        wrong_size.file_size = 8192;
        assert!(run(r.path(), 900, Some(wrong_size)).await.is_err());

        let mut wrong_type = meta();
        wrong_type.file_type = FileType::File;
        assert!(run(r.path(), 900, Some(wrong_type)).await.is_err());

        let mut other_owner = meta();
        other_owner.uploader_id = 43;
        assert!(run(r.path(), 900, Some(other_owner)).await.is_err());
    }

    #[tokio::test]
    async fn a_callback_reporting_another_file_id_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        assert!(run(r.path(), 901, Some(meta())).await.is_err());
    }

    /// 会话还在但没完成：没有可核对的依据。
    #[tokio::test]
    async fn a_callback_without_a_completed_session_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        let _s = UploadSession::open(r.path(), 42, UPLOAD_ID).expect("open");
        assert!(run(r.path(), 900, Some(meta())).await.is_err());
    }

    /// 🔴 会话已被清理 / 从未存在：拒绝，且**不得**因为这次查询建出目录。
    #[tokio::test]
    async fn a_callback_for_a_vanished_session_is_refused_without_creating_it() {
        let r = tempfile::tempdir().expect("tmp");
        let err = run(r.path(), 900, Some(meta())).await.expect_err("必须失败");
        assert!(err.is_session_gone());
        assert!(
            !r.path().join("42").join(UPLOAD_ID).exists(),
            "回调不得惰性建出会话目录"
        );
    }

    #[tokio::test]
    async fn a_callback_pointing_at_a_missing_row_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        assert!(run(r.path(), 900, None).await.is_err());
    }

    /// 🔴 **基础设施故障不是客户端错误。**
    ///
    /// 数据库暂时读不到时，把它标成 validation 等于告诉调用方「别重试了」——
    /// 一次抖动就把这次回调永久判死。这是「可重试性分类」那类错误的又一处实例。
    #[tokio::test]
    async fn a_database_failure_is_internal_not_a_rejection() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);

        let err = authorise_callback(r.path(), &token(), 900, |_| async move {
            Err("connection reset by peer".to_string())
        })
        .await
        .expect_err("必须失败");
        assert!(
            !err.is_rejected(),
            "数据库故障必须是 internal（可重试），实际: {err:?}"
        );
    }

    /// 🔴 损坏的 `state.json` 是**持久**故障：重复回调不会让它变好。
    /// 标成 internal 会让客户端对着一个永不自愈的状态死循环；标成 validation
    /// 又会让它永久放弃。正确结局是第三种：重新 prepare 从头传。
    #[tokio::test]
    async fn a_corrupted_session_state_tells_the_client_to_start_over() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        std::fs::write(
            r.path().join("42").join(UPLOAD_ID).join("state.json"),
            b"{ this is not json",
        )
        .expect("corrupt it");

        let err = run(r.path(), 900, Some(meta())).await.expect_err("必须失败");
        assert!(
            err.is_session_gone(),
            "状态损坏必须是 SessionGone（重新 prepare），实际: {err:?}"
        );
    }

    /// 会话目录不存在同样是「重来一遍」，不是「你参数错了」——
    /// 后者会让客户端永久放弃这次发送。
    #[tokio::test]
    async fn a_vanished_session_tells_the_client_to_start_over() {
        let r = tempfile::tempdir().expect("tmp");
        let err = run(r.path(), 900, Some(meta())).await.expect_err("必须失败");
        assert!(
            err.is_session_gone(),
            "会话不存在必须是 SessionGone，实际: {err:?}"
        );
    }

    /// 三类互斥的完整口径，防止「一律某一类」这种偷懒修法。
    #[tokio::test]
    async fn the_three_outcomes_stay_distinct() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);

        // 抖动 → 可重试
        let transient = authorise_callback(r.path(), &token(), 900, |_| async move {
            Err("connection reset".to_string())
        })
        .await
        .expect_err("must fail");
        assert!(transient.is_internal());

        // 身份不符 → 别重试
        let mut wrong = meta();
        wrong.file_hash = Some("b".repeat(64));
        let rejected = run(r.path(), 900, Some(wrong)).await.expect_err("must fail");
        assert!(rejected.is_rejected());

        // 会话没了 → 重来一遍
        let empty = tempfile::tempdir().expect("tmp");
        let gone = run(empty.path(), 900, Some(meta())).await.expect_err("must fail");
        assert!(gone.is_session_gone());
    }

    /// 与上面成对：身份不符是**真正的**拒绝，不能因为怕误伤就一律 internal。
    #[tokio::test]
    async fn an_identity_mismatch_is_a_rejection_not_an_internal_error() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        let mut wrong = meta();
        wrong.file_hash = Some("b".repeat(64));

        let err = run(r.path(), 900, Some(wrong)).await.expect_err("必须失败");
        assert!(err.is_rejected(), "身份不符必须是 validation，实际: {err:?}");
    }

    #[test]
    fn the_tombstone_gate_alone() {
        assert!(check_callback_target_id(Some(900), 900).is_ok());
        assert!(check_callback_target_id(Some(900), 901).is_err());
        assert!(check_callback_target_id(None, 900).is_err());
    }
}
