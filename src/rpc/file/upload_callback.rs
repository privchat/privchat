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

/// 回调的**完整编排**：核对这次报的 `file_id` 确实是本次上传的结果。
///
/// 🔴 **判据是正式文件行与 token 冻结身份的一致性**，会话墓碑只是**加强项**。
///
/// 早先我把「必须有会话墓碑」当成必要条件，结果是：秒传命中根本不创建会话目录，
/// 于是每一次秒传的回调都被拒 —— 而回调失败在 outbox 那边是**终态**，附件被判
/// 永久失败。一个本来什么都不做的完成通知，被我变成了能否决发送的闸门。
///
/// 现在的边界：
/// - 身份不符 → 拒绝（这才是这道闸真正保护的东西：不能对着别人的文件回调）；
/// - 有会话且墓碑与报的 id 不一致 → 拒绝；
/// - 没有会话（秒传命中 / 会话已被清理）→ **不是错误**，凭身份放行；
/// - 会话状态损坏 → 忽略它，同样凭身份放行（它只是加强项，不该反过来阻断）。
async fn authorise_callback<F, Fut>(
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
    // 会话在就用它加强一道；不在不算错。
    // 🔴 `open_existing`：恢复类入口不得惰性建目录，否则「会话早就没了」会被伪装成
    // 「有一个空会话」，还在磁盘上留垃圾。
    let session = crate::service::upload_session::UploadSession::open_existing(
        session_root,
        token.user_id,
        &token.upload_id,
    )
    .map_err(|e| CallbackError::Internal(format!("会话不可读: {e}")))?;
    if let Some(session) = session {
        // 状态损坏时不阻断：墓碑是加强项，凭身份仍可放行。
        if let Ok(Some(expected)) = session.completed_file_id() {
            check_callback_target_id(Some(expected), reported).map_err(CallbackError::Rejected)?;
        }
    }

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

    // 🔴 **不按 purpose 拒绝。**
    //
    // 秒传命中时 SDK 拿到的是 claim token，随后照常调这个回调
    //（`plan_attachment_upload` 两条分支都会走到 `upload_callback`）。
    // 按 `purpose != Upload` 拒绝，等于**每一次秒传都把附件判成发送失败**——
    // 回调失败在 outbox 那边是终态。
    //
    // 这个回调保护的东西由下面的身份核对承担，与 token 用途无关。

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
        token_with_purpose(UploadTokenPurpose::Upload)
    }

    fn token_with_purpose(purpose: UploadTokenPurpose) -> ValidatedUploadToken {
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
            purpose,
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

    /// 会话还在但没完成：墓碑只是加强项，身份对得上就放行。
    ///
    /// （早先这里要求「必须完成」，那正是打断秒传的那条规则。）
    #[tokio::test]
    async fn a_session_without_a_tombstone_still_passes_on_identity() {
        let r = tempfile::tempdir().expect("tmp");
        let _s = UploadSession::open(r.path(), 42, UPLOAD_ID).expect("open");
        assert!(run(r.path(), 900, Some(meta())).await.is_ok());
    }

    /// 🔴 **秒传命中：claim 路径根本不创建会话目录。**
    ///
    /// 这条是那个回归的直接门禁。早先「必须有会话墓碑」的规则会让**每一次秒传**
    /// 的回调被拒——而回调失败在 outbox 那边是终态，附件被判永久失败。
    /// 也就是说：那版一上线，秒传（转发的核心）全线报错。
    #[tokio::test]
    async fn a_callback_after_a_dedup_hit_has_no_session_and_must_still_pass() {
        let r = tempfile::tempdir().expect("tmp");
        // 没有任何会话目录，正是 claim 成功后的样子。
        assert!(
            run(r.path(), 900, Some(meta())).await.is_ok(),
            "秒传命中后的回调必须通过"
        );
        assert!(
            !r.path().join("42").join(UPLOAD_ID).exists(),
            "回调不得惰性建出会话目录"
        );
    }

    /// 🔴 **秒传命中时 SDK 手里的是 claim token**，随后照常调这个回调。
    ///
    /// 按 `purpose != Upload` 拒绝，就是每一次秒传都把附件判成发送失败。
    /// 这条与上一条是同一个回归的两个必要条件，缺一个都会漏。
    #[tokio::test]
    async fn a_claim_token_may_report_its_completion() {
        let r = tempfile::tempdir().expect("tmp");
        let claim = token_with_purpose(UploadTokenPurpose::ClaimExisting);
        let got = authorise_callback(r.path(), &claim, 900, |_| async move { Ok(Some(meta())) })
            .await;
        assert!(got.is_ok(), "claim 用途的 token 必须能完成回调: {got:?}");
    }

    /// 没有会话也**不能**放宽身份：能通过的只是「这份文件确实是我这次要传的」。
    #[tokio::test]
    async fn without_a_session_the_identity_check_still_bites() {
        let r = tempfile::tempdir().expect("tmp");
        let mut other = meta();
        other.uploader_id = 43;
        let err = run(r.path(), 900, Some(other)).await.expect_err("必须失败");
        assert!(err.is_rejected());
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

    /// 损坏的 `state.json` 不该阻断一次身份正确的回调——墓碑只是加强项。
    #[tokio::test]
    async fn a_corrupted_session_state_does_not_block_a_valid_callback() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        std::fs::write(
            r.path().join("42").join(UPLOAD_ID).join("state.json"),
            b"{ this is not json",
        )
        .expect("corrupt it");

        assert!(
            run(r.path(), 900, Some(meta())).await.is_ok(),
            "墓碑是加强项：它坏了不该反过来阻断一次身份正确的回调"
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

        // 会话没了 + 身份对 → 放行（秒传命中就是这样）
        let empty = tempfile::tempdir().expect("tmp");
        assert!(run(empty.path(), 900, Some(meta())).await.is_ok());
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
