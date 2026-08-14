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

/// 回调的**完整编排**：核对这次报的 `file_id` 确实是本次操作的结果。
///
/// 🔴 **两种 purpose 各有自己的精确身份来源，不能互相替代，也不能合并。**
///
/// 一开始我要求「必须有会话墓碑」，那会打断每一次秒传（claim 路径不建会话）。
/// 修的时候我又矫枉过正：干脆不看 purpose、一律凭「内容身份」放行——但**同一份
/// 内容允许有多条独立的逻辑记录**，内容相同证明不了「这条就是这次操作产出的那条」。
/// 而且实体上传因此也被放宽了，它本来是有会话可依的。
///
/// 现在各归各：
/// - `ClaimExisting`：按 `claim_key_hash(raw_token)` 找回**这次 claim** 唯一的
///   `file_id`（`uq_privchat_file_uploads_claim_key` 保证唯一）。不需要会话——
///   claim 本来就不创建会话。
/// - `Upload`：会话墓碑说了算，它记的就是这次上传落库的 `file_id`。
///   会话缺失或损坏 → `SessionGone`，让客户端重新 prepare，而不是凭内容蒙混过去。
///
/// 两条之后再叠一层 `matches_file`（属主/类型/摘要/大小）作纵深防御。
async fn authorise_callback<F, Fut>(
    session_root: &std::path::Path,
    token: &crate::service::upload_token_service::ValidatedUploadToken,
    raw_token: &str,
    reported: u64,
    lookup: &dyn CallbackLookup,
    fetch_meta: F,
) -> Result<crate::service::file_service::FileMetadata, CallbackError>
where
    F: FnOnce(u64) -> Fut,
    Fut: std::future::Future<
        Output = Result<Option<crate::service::file_service::FileMetadata>, String>,
    >,
{
    use crate::service::upload_token_service::UploadTokenPurpose;

    let expected = match token.purpose {
        UploadTokenPurpose::ClaimExisting => {
            // 这次 claim 的精确结果：幂等键由**收到的那份凭证**派生。
            let key = crate::service::file_claim_service::claim_key_hash(raw_token);
            lookup
                .find_claimed(token.user_id, &key)
                .await
                .map_err(CallbackError::Internal)?
                .ok_or(CallbackError::Rejected("这次秒传没有产生记录"))?
        }
        UploadTokenPurpose::Upload => {
            // 🔴 `open_existing`：恢复类入口不得惰性建目录。
            let session = crate::service::upload_session::UploadSession::open_existing(
                session_root,
                token.user_id,
                &token.upload_id,
            )
            .map_err(|e| CallbackError::Internal(format!("会话不可读: {e}")))?
            .ok_or(CallbackError::SessionGone("该次上传的会话已不存在"))?;

            session
                .completed_file_id()
                .map_err(|_| CallbackError::SessionGone("会话状态已损坏"))?
                .ok_or(CallbackError::Rejected("该次上传尚未完成，无法回调"))?
        }
    };

    check_callback_target_id(Some(expected), reported).map_err(CallbackError::Rejected)?;

    let meta = fetch_meta(reported)
        .await
        .map_err(|e| CallbackError::Internal(format!("读取文件记录失败: {e}")))?
        .ok_or(CallbackError::Rejected("file_id 不存在"))?;
    if !token.matches_file(&meta) {
        return Err(CallbackError::Rejected("file_id 与本次上传的身份不符"));
    }
    Ok(meta)
}

/// claim 幂等记录的查询口。抽成 trait 只为让编排能被测试驱动。
#[async_trait::async_trait]
pub(crate) trait CallbackLookup: Sync {
    async fn find_claimed(&self, uploader_id: u64, key: &str) -> Result<Option<u64>, String>;
}

struct RepoLookup(std::sync::Arc<crate::service::file_service::FileService>);

#[async_trait::async_trait]
impl CallbackLookup for RepoLookup {
    async fn find_claimed(&self, uploader_id: u64, key: &str) -> Result<Option<u64>, String> {
        self.0
            .find_claimed(uploader_id, key)
            .await
            .map_err(|e| e.to_string())
    }
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
    let lookup = RepoLookup(services.file_service.clone());
    authorise_callback(
        &session_root,
        &token_info,
        upload_token,
        file_id_num,
        &lookup,
        |id| async move {
            file_service
                .get_file_metadata(id)
                .await
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| match e {
        CallbackError::Rejected(reason) => {
            warn!("❌ 上传回调被拒: {reason}（file_id={file_id_num}）");
            RpcError::validation(reason.to_string())
        }
        // 🔴 基础设施故障必须是 internal：标成 validation 等于告诉客户端
        // 「别重试了」，一次数据库抖动就把这次回调永久判死。
        // 🔴 **兼容期映射：会话丢失要让客户端「重跑整条流程」，而不是永久失败。**
        //
        // 语义上这是 `ResourceNotFound`，但**已发布的 SDK 不认这个码**：
        // `is_retryable_server_code` 不含它，outbox 会把附件判成终态失败并丢掉密文缓存。
        //
        // 而「重跑整条 outbox 流程」恰好就是我们要的恢复动作——它会重新 prepare、
        // 复用磁盘上那份 sealed blob 再传一次。所以这里回 `ServiceUnavailable`：
        // 对现网 SDK 而言语义正确、行为正确，零客户端改动。
        //
        // 📌 等 SDK 学会认 `ResourceNotFound` 并主动重新 prepare 后，换回 not_found；
        // 在那之前**不要**改，否则一次会话丢失就是一条附件永久发不出去。
        CallbackError::SessionGone(reason) => {
            warn!("🔁 上传会话不可恢复: {reason}（file_id={file_id_num}），让客户端重跑整条流程");
            RpcError::from_code(privchat_protocol::ErrorCode::ServiceUnavailable, reason.to_string())
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
    use super::{authorise_callback, check_callback_target_id, CallbackLookup};
    use crate::model::file_upload::FileType;
    use crate::service::file_service::FileMetadata;
    use crate::service::upload_session::UploadSession;
    use crate::service::upload_token_service::{
        UploadIdentity, UploadToken, UploadTokenPurpose, ValidatedUploadToken,
    };

    const UPLOAD_ID: &str = "b3f1a2c40000400080000000000000ff";
    const RAW_TOKEN: &str = "b3f1a2c4-0000-4000-8000-000000000001";

    /// claim 幂等记录的假实现。
    ///
    /// 🔴 它**校验查询键**：早先这个 fake 忽略 key，于是把生产代码里的
    /// `claim_key_hash(raw_token)` 换成 `upload_id`、换成常量，测试照样全绿——
    /// 只证明了「lookup 返回的 id 会被采用」，没证明「查的是这次 claim」。
    struct Claims(Option<u64>);

    #[async_trait::async_trait]
    impl CallbackLookup for Claims {
        async fn find_claimed(&self, uploader: u64, key: &str) -> Result<Option<u64>, String> {
            assert_eq!(uploader, 42, "必须按 token 里的 uploader 查");
            assert_eq!(
                key,
                crate::service::file_claim_service::claim_key_hash(RAW_TOKEN),
                "幂等键必须由**客户端提交的原始 token** 派生"
            );
            Ok(self.0)
        }
    }

    struct BrokenClaims;

    #[async_trait::async_trait]
    impl CallbackLookup for BrokenClaims {
        async fn find_claimed(&self, _uploader: u64, _key: &str) -> Result<Option<u64>, String> {
            Err("connection reset".to_string())
        }
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
        let mut v = ValidatedUploadToken::from_legacy(RAW_TOKEN, &record);
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

    fn completed_session(root: &std::path::Path, file_id: u64) {
        let s = UploadSession::open(root, 42, UPLOAD_ID).expect("open");
        s.mark_completed(file_id).expect("mark");
    }

    async fn run_upload(
        root: &std::path::Path,
        reported: u64,
        found: Option<FileMetadata>,
    ) -> Result<FileMetadata, super::CallbackError> {
        let lookup = Claims(None);
        authorise_callback(
            root,
            &token_with_purpose(UploadTokenPurpose::Upload),
            RAW_TOKEN,
            reported,
            &lookup,
            |_| async move { Ok(found) },
        )
        .await
    }

    async fn run_claim(
        root: &std::path::Path,
        claimed: Option<u64>,
        reported: u64,
        found: Option<FileMetadata>,
    ) -> Result<FileMetadata, super::CallbackError> {
        let lookup = Claims(claimed);
        authorise_callback(
            root,
            &token_with_purpose(UploadTokenPurpose::ClaimExisting),
            RAW_TOKEN,
            reported,
            &lookup,
            |_| async move { Ok(found) },
        )
        .await
    }

    // ---------- 实体上传 ----------

    #[tokio::test]
    async fn an_upload_callback_matching_its_tombstone_is_accepted() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        assert!(run_upload(r.path(), 900, Some(meta())).await.is_ok());
    }

    #[tokio::test]
    async fn an_upload_callback_reporting_another_file_id_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        assert!(run_upload(r.path(), 901, Some(meta())).await.is_err());
    }

    /// 🔴 实体上传**有**会话可依，不该因为修秒传而被放宽：会话没了就重来一遍。
    #[tokio::test]
    async fn an_upload_without_a_session_must_start_over() {
        let r = tempfile::tempdir().expect("tmp");
        let err = run_upload(r.path(), 900, Some(meta())).await.expect_err("必须失败");
        assert!(err.is_session_gone(), "实际: {err:?}");
        assert!(
            !r.path().join("42").join(UPLOAD_ID).exists(),
            "回调不得惰性建出会话目录"
        );
    }

    #[tokio::test]
    async fn an_upload_with_a_corrupted_session_must_start_over() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        std::fs::write(
            r.path().join("42").join(UPLOAD_ID).join("state.json"),
            b"{ not json",
        )
        .expect("corrupt");
        let err = run_upload(r.path(), 900, Some(meta())).await.expect_err("必须失败");
        assert!(err.is_session_gone(), "实际: {err:?}");
    }

    // ---------- 秒传 ----------

    /// 🔴 claim 路径**根本不创建会话**。要求会话就是把每一次秒传判成发送失败——
    /// 而回调失败在 outbox 那边是终态。
    #[tokio::test]
    async fn a_claim_callback_needs_no_session() {
        let r = tempfile::tempdir().expect("tmp");
        assert!(run_claim(r.path(), Some(900), 900, Some(meta())).await.is_ok());
        assert!(!r.path().join("42").join(UPLOAD_ID).exists());
    }

    /// 🔴 但**不能**退化成「内容相同就行」：同一份内容允许有多条独立记录，
    /// 判据必须是**这次 claim** 产生的那一条（`claim_key_hash` 唯一确定）。
    #[tokio::test]
    async fn a_claim_callback_must_name_the_row_this_claim_created() {
        let r = tempfile::tempdir().expect("tmp");
        // 这次 claim 产生的是 900；调用方却报同用户、同内容的另一条 901。
        let mut same_content = meta();
        same_content.file_id = 901;
        let err = run_claim(r.path(), Some(900), 901, Some(same_content))
            .await
            .expect_err("同内容的另一条记录必须被拒");
        assert!(err.is_rejected(), "实际: {err:?}");
    }

    #[tokio::test]
    async fn a_claim_callback_without_a_claim_record_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        let err = run_claim(r.path(), None, 900, Some(meta())).await.expect_err("必须失败");
        assert!(err.is_rejected(), "实际: {err:?}");
    }

    #[tokio::test]
    async fn a_failing_claim_lookup_is_internal_not_a_rejection() {
        let r = tempfile::tempdir().expect("tmp");
        let err = authorise_callback(
            r.path(),
            &token_with_purpose(UploadTokenPurpose::ClaimExisting),
            RAW_TOKEN,
            900,
            &BrokenClaims,
            |_| async move { Ok(Some(meta())) },
        )
        .await
        .expect_err("必须失败");
        assert!(err.is_internal(), "实际: {err:?}");
    }

    // ---------- 共通 ----------

    #[tokio::test]
    async fn a_callback_whose_file_does_not_match_the_token_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        for mutate in [
            (|m: &mut FileMetadata| m.file_hash = Some("b".repeat(64))) as fn(&mut FileMetadata),
            |m: &mut FileMetadata| m.file_size = 8192,
            |m: &mut FileMetadata| m.file_type = FileType::File,
            |m: &mut FileMetadata| m.uploader_id = 43,
        ] {
            let mut wrong = meta();
            mutate(&mut wrong);
            assert!(run_upload(r.path(), 900, Some(wrong)).await.is_err());
        }
    }

    #[tokio::test]
    async fn a_callback_pointing_at_a_missing_row_is_refused() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        assert!(run_upload(r.path(), 900, None).await.is_err());
    }

    #[tokio::test]
    async fn a_database_failure_is_internal_not_a_rejection() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        let lookup = Claims(None);
        let err = authorise_callback(
            r.path(),
            &token_with_purpose(UploadTokenPurpose::Upload),
            RAW_TOKEN,
            900,
            &lookup,
            |_| async move { Err("connection reset".to_string()) },
        )
        .await
        .expect_err("必须失败");
        assert!(err.is_internal(), "实际: {err:?}");
    }

    /// 三类结局互斥，防止将来被悄悄合并。
    #[tokio::test]
    async fn the_three_outcomes_stay_distinct() {
        let r = tempfile::tempdir().expect("tmp");
        completed_session(r.path(), 900);
        let lookup = Claims(None);

        // 抖动 → 可重试
        let transient = authorise_callback(
            r.path(),
            &token_with_purpose(UploadTokenPurpose::Upload),
            RAW_TOKEN,
            900,
            &lookup,
            |_| async move { Err("boom".to_string()) },
        )
        .await
        .expect_err("must fail");
        assert!(transient.is_internal());

        // 身份不符 → 别重试
        let mut wrong = meta();
        wrong.file_hash = Some("b".repeat(64));
        assert!(run_upload(r.path(), 900, Some(wrong))
            .await
            .expect_err("must fail")
            .is_rejected());

        // 会话没了 → 重来一遍
        let empty = tempfile::tempdir().expect("tmp");
        assert!(run_upload(empty.path(), 900, Some(meta()))
            .await
            .expect_err("must fail")
            .is_session_gone());
    }

    #[test]
    fn the_tombstone_gate_alone() {
        assert!(check_callback_target_id(Some(900), 900).is_ok());
        assert!(check_callback_target_id(Some(900), 901).is_err());
        assert!(check_callback_target_id(None, 900).is_err());
    }
}
