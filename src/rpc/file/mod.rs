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

//! 文件相关 RPC 接口

pub mod get_url;
pub mod claim_existing;
pub mod request_chunked_upload_token;
pub mod request_upload_token;
pub mod upload_callback;
pub mod validate_token;

pub use get_url::get_file_url;
pub use claim_existing::claim_existing;
pub use request_chunked_upload_token::request_chunked_upload_token;
pub use request_upload_token::request_upload_token;
pub use upload_callback::upload_callback;
pub use validate_token::validate_upload_token;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册文件系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // 客户端 RPC（公开接口）
    let services1 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/request_upload_token", move |params, ctx| {
            let services = services1.clone();
            Box::pin(async move { request_upload_token(services, params, ctx).await })
        })
        .await;

    // 分片上传：独立 RPC（RESUMABLE_UPLOAD_SPEC §2），与整包互不影响。
    let services_chunked = services.clone();
    GLOBAL_RPC_ROUTER
        .register(
            privchat_protocol::rpc::routes::file::REQUEST_CHUNKED_UPLOAD_TOKEN,
            move |params, ctx| {
                let services = services_chunked.clone();
                Box::pin(async move { request_chunked_upload_token(services, params, ctx).await })
            },
        )
        .await;

    // 秒传命中后取得所有权：与探测分开的独立入口（见 claim_existing 模块说明）。
    let services_claim = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/claim_existing", move |params, ctx| {
            let services = services_claim.clone();
            Box::pin(async move { claim_existing(services, params, ctx).await })
        })
        .await;

    // 内部 RPC（仅文件服务器调用）
    let services2 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/validate_token", move |params, _ctx| {
            let services = services2.clone();
            Box::pin(async move { validate_upload_token(services, params).await })
        })
        .await;

    let services3 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/upload_callback", move |params, ctx| {
            let services = services3.clone();
            Box::pin(async move { upload_callback(services, params, ctx).await })
        })
        .await;

    let services4 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/get_url", move |params, ctx| {
            let services = services4.clone();
            Box::pin(async move { get_file_url(services, params, ctx).await })
        })
        .await;

    tracing::debug!("📁 File 系统路由注册完成");
}

/// 下发给客户端的附件密钥（v2）。
///
/// 🔴 只在**已鉴权**的响应里出现，绝不进 URL、不进日志——威胁模型是对象存储
/// 服务商，密钥必须只走我们自己的接口（ATTACHMENT_ENCRYPTION_SPEC §0.1）。
pub(crate) fn attachment_key_for(
    config: &crate::config::ServerConfig,
    key_id: Option<u8>,
) -> Option<privchat_protocol::rpc::file::upload::AttachmentKey> {
    let key_id = key_id?;
    config
        .attachment_keys
        .iter()
        .find(|(id, _)| *id == key_id)
        .map(|(id, key)| privchat_protocol::rpc::file::upload::AttachmentKey {
            key_id: *id,
            key: key.clone(),
        })
}

/// 本次上传该用的密钥 = 列表第一项。其余是保留给老对象解密的。
pub(crate) fn current_attachment_key(
    config: &crate::config::ServerConfig,
) -> Option<privchat_protocol::rpc::file::upload::AttachmentKey> {
    config
        .attachment_keys
        .first()
        .map(|(id, key)| privchat_protocol::rpc::file::upload::AttachmentKey {
            key_id: *id,
            key: key.clone(),
        })
}

#[cfg(test)]
mod attachment_key_tests {
    use super::{attachment_key_for, current_attachment_key};
    use crate::config::{AttachmentKeys, ServerConfig};

    fn cfg(keys: Vec<(u8, &str)>) -> ServerConfig {
        ServerConfig {
            attachment_keys: AttachmentKeys(
                keys.into_iter().map(|(i, k)| (i, k.to_string())).collect(),
            ),
            ..ServerConfig::default()
        }
    }

    /// 未配置 = v2 关闭，客户端沿用 v1。服务端绝不凭空造一把——
    /// 那会让新旧对象都解不开。
    #[test]
    fn no_configured_key_means_v2_is_off() {
        assert!(current_attachment_key(&cfg(vec![])).is_none());
        assert!(attachment_key_for(&cfg(vec![]), Some(1)).is_none());
    }

    /// 上传永远用第一把；其余是保留给存量对象解密的。
    #[test]
    fn uploads_always_use_the_first_key() {
        let c = cfg(vec![(2, "current"), (1, "retired")]);
        let k = current_attachment_key(&c).expect("有当前密钥");
        assert_eq!((k.key_id, k.key.as_str()), (2, "current"));
    }

    /// 🔴 下载只给**这一个文件**用的那把。
    ///
    /// 下发全量密钥表意味着任何拿到一个附件的人就获得了全部历史对象的解密能力——
    /// 鉴权挡住的是「这个文件」，密钥的暴露面就该止步于此。
    #[test]
    fn a_download_only_receives_the_key_for_that_object() {
        let c = cfg(vec![(2, "current"), (1, "retired")]);

        let k = attachment_key_for(&c, Some(1)).expect("按 id 取到");
        assert_eq!((k.key_id, k.key.as_str()), (1, "retired"));

        let k = attachment_key_for(&c, Some(2)).expect("按 id 取到");
        assert_eq!(k.key_id, 2);
    }

    /// 非 v2 的行（明文 / per-file CEK）不带 key_id，不该拿到任何全站密钥。
    #[test]
    fn non_v2_objects_get_no_site_key() {
        let c = cfg(vec![(1, "k")]);
        assert!(attachment_key_for(&c, None).is_none());
    }

    /// 已退役、配置里已删掉的 key_id 必须返回 None 而不是回落到当前密钥——
    /// 拿错密钥解密会失败，但那时错误已经离现场很远了。
    #[test]
    fn a_retired_key_id_is_not_silently_replaced() {
        let c = cfg(vec![(2, "current")]);
        assert!(attachment_key_for(&c, Some(1)).is_none());
    }

    /// 🔴 密钥不得出现在 Debug 输出里。ServerConfig 派生了 Debug，
    /// 一次 `{:?}` 就能把它打进日志。
    #[test]
    fn keys_never_render_in_debug_output() {
        let c = cfg(vec![(1, "super-secret-material")]);
        let rendered = format!("{:?}", c.attachment_keys);
        assert!(!rendered.contains("super-secret-material"), "{rendered}");
        assert!(rendered.contains("REDACTED"), "{rendered}");

        let whole = format!("{:?}", c);
        assert!(!whole.contains("super-secret-material"), "整个配置也不得泄露");

        let k = current_attachment_key(&c).unwrap();
        let rendered = format!("{k:?}");
        assert!(!rendered.contains("super-secret-material"), "{rendered}");
    }

    /// 密钥不得被序列化进配置 dump。
    #[test]
    fn keys_are_excluded_from_serialization() {
        let c = cfg(vec![(1, "super-secret-material")]);
        let json = serde_json::to_string(&c).expect("serialize");
        assert!(!json.contains("super-secret-material"), "密钥进了序列化输出");
    }
}

/// 签发 token 时冻结的加密参数。
///
/// 🔴 全部由**服务端**决定：客户端不选格式、不选密钥、不选块大小。让客户端在
/// complete 时重新提供任何一项，就等于让被检查的一方来定检查标准。
///
/// 🔴 分块几何必须冻结的具体原因：同一份明文按不同块大小封装会得到**不同长度**的
/// 密文（每块多一个 nonce 和一个 tag），token 里签的 `sealed_size` 就对不上，
/// complete 会把一次正常上传判成身份不符。
pub(crate) struct FrozenCrypto {
    pub format_version: u8,
    pub encryption_key_id: u8,
    pub chunk_plain_size: u32,
    /// 按明文大小与分块几何算出的密文字节数。
    pub sealed_size: i64,
}

/// 为一次上传冻结加密参数。
///
/// 🔴 **没有配置附件密钥时直接报错，不回退明文。** 用「没有密钥就当明文」表达
/// 未启用是 fail-open：一次配置遗漏就会让全部新附件以明文进桶，而桶里看起来一切正常，
/// 没有任何报错会提醒运维。
pub(crate) fn freeze_crypto(
    config: &crate::config::ServerConfig,
    plaintext_size: i64,
) -> Result<FrozenCrypto, String> {
    use privchat_protocol::attachment_crypto as ac;

    let (key_id, _) = config
        .attachment_keys
        .first()
        .ok_or_else(|| "服务端未配置附件加密密钥（[[attachment.keys]]）".to_string())?;

    // 🔴 负数**拒绝**，不是夹到 0。
    //
    // `max(0)` 把一个非法请求（明文大小 -1）悄悄变成一个合法冻结：token 会签下
    // "明文 0 字节"，而客户端心里想的是别的东西。签完之后这套参数就是权威身份，
    // 后面每一步都按它算——错误在此刻不报，就只会在 complete 的校验里以
    // 「身份不符」的面目出现，离真正的原因隔了整整一条链路。
    if plaintext_size < 0 {
        return Err(format!("明文大小不能是负数: {plaintext_size}"));
    }
    let chunk_plain_size = ac::DEFAULT_CHUNK_PLAIN_SIZE;
    let sealed = ac::sealed_len(plaintext_size as u64, chunk_plain_size)?;

    Ok(FrozenCrypto {
        format_version: ac::FORMAT_VERSION,
        encryption_key_id: *key_id,
        chunk_plain_size,
        sealed_size: sealed as i64,
    })
}
