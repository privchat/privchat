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

use privchat_protocol::rpc::file::upload::FileGetUrlRequest;
use privchat_protocol::rpc::file::upload::FileGetUrlResponse;
use serde_json::Value;

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcContext;
use crate::rpc::RpcServiceContext;

/// 处理获取文件 URL 请求
pub async fn get_file_url(
    services: RpcServiceContext,
    params: Value,
    _ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&_ctx)?;

    let request: FileGetUrlRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("参数错误: {}", e)))?;

    tracing::info!(
        "🔗 获取文件 URL: file_id={}, user_id={}",
        request.file_id,
        user_id
    );

    // 附件访问授权（MEDIA_REFERENCE_AND_FORWARD_SPEC §4.1）：本接口返回 CEK，
    // 必须校验访问权，否则任意登录用户拿 file_id 即可解密。注意：cek 绝不进日志。
    //
    // 判据是**存在性**，不是单点绑定：只要存在一条引用该文件、且未删除未撤回的消息，
    // 请求者又是那条消息所在会话的成员，就放行。转发副本靠的就是这一条。
    //
    // 候选消息有两条发现路径，**判据只有一套**：
    //   1. 引用表（权威）
    //   2. 老的 business_id 单点绑定（过渡期，存量回填前的兜底）
    // 🔴 第 2 条只是「怎么找到候选消息」的另一种方式，**不是**回落到旧的
    // authorize_file_access 语义——否则 §4.2 那个「撤回后附件仍可下载」的洞会原样留着。
    let file_meta = services
        .file_service
        .get_file_metadata(request.file_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询文件失败: {}", e)))?
        .ok_or_else(|| RpcError::validation("文件不存在".to_string()))?;

    // 🔴 判定失败是 5xx，不是 403：把「服务异常」映射成「无权」会让用户
    // 去申请权限，而真实原因是数据库抖了一下；更糟的是反过来——旧实现把
    // 查询失败吞成空结果，文件被当成 pending，于是上传者反而拿得到 CEK。
    let decision = crate::service::attachment_authorization::resolve_attachment_access(
        &services.message_repository,
        &services.channel_service,
        &file_meta,
        user_id,
    )
    .await
    .map_err(|error| {
        // 🔴 底层错误原文不回客户端：router 会把 message 原样透传，
        // 数据库连接串、表名、SQL 片段就这样漏到了外面。
        // 详情进服务端日志，客户端只拿一个稳定标识。
        tracing::error!(
            "附件授权判定不可用 file_id={} user_id={}: {}",
            request.file_id,
            user_id,
            error
        );
        RpcError::internal("ATTACHMENT_AUTHORIZATION_UNAVAILABLE".to_string())
    })?;

    if decision.authorized {
        // fallback 命中率是「回填够不够」的唯一读数。归零之前不能删掉第 2 条发现路径，
        // 归零之后才谈得上移除 business_id 兼容（spec §10 第 9 步）。
        crate::infra::metrics::record_file_access_authorized(decision.source);
    } else {
        tracing::warn!(
            "🚫 拒绝访问附件: file_id={}, user_id={}, candidates={}, source={:?}",
            request.file_id,
            user_id,
            decision.candidate_count,
            decision.source
        );
        crate::infra::metrics::record_file_access_denied();
        return Err(RpcError::forbidden("无权访问该附件".to_string()));
    }

    // 🔴 顺序有意义：**先**取密钥、取不到就直接返回，再去要 URL。
    // 反过来的话，一次注定不可用的响应还会先去签一个能下载密文的地址。
    let attachment_key = attachment_key_or_fail(&services.config, &file_meta)?;

    let url = services
        .file_service
        .get_file_url(request.file_id, user_id)
        .await
        .map_err(|e| RpcError::internal(format!("获取文件 URL 失败: {}", e)))?;

    // 🔴 第二十六轮评审：日志最小化——访问 URL 不进日志，只记 file_id。
    tracing::info!("🔗 返回文件 URL: file_id={}", request.file_id);

    let response = get_url_response(file_meta, url, attachment_key);

    Ok(serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化失败: {}", e)))?)
}

/// 取这个对象要用的那把全站密钥；取不到就 fail-closed。
///
/// 🔴 判据是 `format_version == FORMAT_VERSION`，**不是写死的数字**。
/// 这里曾经写 `== 2`——那是"加密版本"旧口径的遗留值，而密文格式版本是 1
/// （`attachment_crypto::FORMAT_VERSION`，数据库 CHECK 也钉着 1）。于是这道闸门对
/// 库里每一个对象都不成立：密钥漏配时一次都不触发，照常回一个没有密钥的密文 URL，
/// 故障表现成"图片坏了"，离真实成因十万八千里。fail-closed 写成了恒不生效。
fn attachment_key_or_fail(
    config: &crate::config::ServerConfig,
    file_meta: &crate::service::FileMetadata,
) -> Result<Option<privchat_protocol::rpc::file::upload::AttachmentKey>, RpcError> {
    let key = super::attachment_key_for(config, Some(file_meta.object.encryption_key_id));
    if file_meta.object.format_version == privchat_protocol::attachment_crypto::FORMAT_VERSION
        && key.is_none()
    {
        tracing::error!(
            "附件密钥缺失: file_id={} encryption_key_id={}——配置里已无此 key id",
            file_meta.file_id,
            file_meta.object.encryption_key_id
        );
        return Err(RpcError::internal("ATTACHMENT_KEY_UNAVAILABLE".to_string()));
    }
    Ok(key)
}

/// 组装响应。抽出来是为了让"哪个摘要进 `sha256`"这件事可以被直接断言——
/// 它埋在 RPC 里的时候，只有一条要连库的路径能碰到它。
fn get_url_response(
    file_meta: crate::service::FileMetadata,
    url: crate::service::file_service::FileUrlResponse,
    attachment_key: Option<privchat_protocol::rpc::file::upload::AttachmentKey>,
) -> FileGetUrlResponse {
    FileGetUrlResponse {
        file_url: url.file_url,
        expires_at: url.expires_at,
        file_size: url.file_size as u64,
        mime_type: url.mime_type,
        // 文件名取自 file 表（已在上方鉴权时拉取的 file_meta），统一由 get_url 下发。
        original_filename: file_meta.original_filename,
        // 这个对象的 `encryption_key_id` 对应的那把**全站**密钥。
        //
        // 🔴 它不是 per-file key，别按"密钥暴露面止步于这个文件"去理解。同一个
        // key_id 下的所有对象共用这一把——拿到它的人，密码学上就能解开那一批。
        // 文件级的隔离来自别处：私有桶 + 短期 URL + `get_url` 的鉴权。
        //
        // 仍然只下发这一把、不下发全量密钥表：那样至少把暴露面限制在**一代**密钥上，
        // 轮换之后旧对象不会跟着一起泄。这是纵深防御的一层，不是隔离本身。
        attachment_key,
        // 转发同一份附件时，客户端拿它直接走 prepare + claim。
        //
        // 🔴 必须是**明文**摘要：prepare/claim 的判重键就是它（`converge_upload`
        // 按 plaintext_sha256 收敛）。这里回密文摘要的话，客户端拿去 prepare 永远
        // 命不中——而且是"稳定命不中"：转发一份已经在库里的附件，每次都会重新上传
        // 一遍，秒传对转发这条最该生效的路径完全失效。
        plaintext_sha256: Some(file_meta.object.plaintext_sha256),
        // 真实类型由服务端下发，客户端不该按 mime 猜——猜出来的表还会在每个
        // 端各存一份。
        file_type: file_meta.file_type.as_str().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AttachmentKeys, ServerConfig};
    use crate::model::file_upload::{AttachmentObject, FileType};
    use crate::service::FileMetadata;
    use privchat_protocol::attachment_crypto as ac;

    const PLAIN: &str = "11111111111111111111111111111111111111111111111111111111111111aa";
    const SEALED: &str = "22222222222222222222222222222222222222222222222222222222222222bb";

    fn meta(key_id: u8, format_version: u8) -> FileMetadata {
        FileMetadata {
            file_id: 900,
            original_filename: "photo.png".to_string(),
            original_size: None,
            file_type: FileType::Image,
            mime_type: "image/png".to_string(),
            uploader_id: 42,
            uploader_ip: None,
            uploaded_at: 0,
            width: None,
            height: None,
            business_type: Some("message".to_string()),
            business_id: None,
            object: AttachmentObject {
                object_id: 7,
                // 🔴 两个摘要**故意不同**：相同的话，"回错了哪一个"根本测不出来。
                plaintext_sha256: PLAIN.to_string(),
                plaintext_size: 4096,
                sealed_sha256: SEALED.to_string(),
                sealed_size: 4164,
                file_path: "images/900.png".to_string(),
                storage_source_id: 0,
                format_version,
                encryption_key_id: key_id,
            },
        }
    }

    fn config_with(key_ids: &[u8]) -> ServerConfig {
        ServerConfig {
            attachment_keys: AttachmentKeys(
                key_ids
                    .iter()
                    .enumerate()
                    .map(|(i, id)| {
                        // 每个 id 一把不同的密钥（内容不同是配置校验的硬要求）。
                        (*id, format!("{}{}", "A".repeat(42), (b'a' + i as u8) as char))
                    })
                    .collect(),
            ),
            ..ServerConfig::default()
        }
    }

    /// 🔴 转发靠的是**明文**摘要：客户端拿响应里的 `sha256` 直接去 prepare + claim，
    /// 而判重键是 plaintext_sha256。这里回密文摘要的话，转发会稳定秒传不中——
    /// 每转发一次就重传一份，且没有任何报错。
    #[test]
    fn the_response_carries_the_plaintext_digest_not_the_sealed_one() {
        let url = crate::service::file_service::FileUrlResponse {
            file_url: "https://cdn/x".to_string(),
            thumbnail_url: None,
            expires_at: 0,
            file_size: 4164,
            mime_type: "image/png".to_string(),
            storage_source_id: 0,
        };
        let resp = get_url_response(meta(1, ac::FORMAT_VERSION), url, None);
        assert_eq!(resp.plaintext_sha256.as_deref(), Some(PLAIN), "必须是明文摘要");
        assert_ne!(resp.plaintext_sha256.as_deref(), Some(SEALED), "绝不能回密文摘要");
    }

    /// 🔴 密钥漏配 → 稳定的 `ATTACHMENT_KEY_UNAVAILABLE`，而且**发生在要 URL 之前**。
    ///
    /// 照常返回 URL 只会让客户端下到一堆解不开的密文，故障表现成"图片坏了"。
    #[test]
    fn a_missing_key_fails_closed_with_a_stable_marker() {
        // 对象用 key_id=2，配置里只有 1。
        let err = attachment_key_or_fail(&config_with(&[1]), &meta(2, ac::FORMAT_VERSION))
            .expect_err("密钥缺失必须拒绝");
        assert!(
            err.to_string().contains("ATTACHMENT_KEY_UNAVAILABLE"),
            "标识必须稳定可监控: {err}"
        );
    }

    /// 🔴 这条盯的是**判据本身**：闸门必须对"当前格式版本"的对象生效。
    ///
    /// 它曾经写成 `format_version == 2`，而当前格式版本是 1，于是对库里每一个对象
    /// 都不成立——fail-closed 恒不触发。把版本换成当前值之后这条才谈得上有意义，
    /// 所以这里直接钉住 `FORMAT_VERSION`：谁再把它改成某个具体数字，这条就红。
    #[test]
    fn the_guard_applies_to_objects_of_the_current_format_version() {
        assert_eq!(ac::FORMAT_VERSION, 1, "格式版本变了就要重新检查这道闸门");
        assert!(
            attachment_key_or_fail(&config_with(&[9]), &meta(1, ac::FORMAT_VERSION)).is_err(),
            "当前格式版本的对象缺密钥必须拒绝"
        );
        // 配得上就照常放行，并且给的是这个对象那一把。
        let key = attachment_key_or_fail(&config_with(&[1, 2]), &meta(2, ac::FORMAT_VERSION))
            .expect("配置里有这把 key")
            .expect("必须下发");
        assert_eq!(key.key_id, 2, "下发的必须是这个对象记录的那一把");
    }
}
