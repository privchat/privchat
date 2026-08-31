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
pub(crate) fn attachment_keys(
    config: &crate::config::ServerConfig,
) -> Vec<privchat_protocol::rpc::file::upload::AttachmentKey> {
    config
        .attachment_keys
        .iter()
        .map(|(id, key)| privchat_protocol::rpc::file::upload::AttachmentKey {
            key_id: *id,
            key: key.clone(),
        })
        .collect()
}

/// 本次上传该用的密钥 = 列表第一项。其余是保留给老对象解密的。
pub(crate) fn current_attachment_key(
    config: &crate::config::ServerConfig,
) -> Option<privchat_protocol::rpc::file::upload::AttachmentKey> {
    attachment_keys(config).into_iter().next()
}

#[cfg(test)]
mod attachment_key_tests {
    use super::{attachment_keys, current_attachment_key};
    use crate::config::ServerConfig;

    fn cfg(keys: Vec<(u8, String)>) -> ServerConfig {
        ServerConfig {
            attachment_keys: keys,
            ..ServerConfig::default()
        }
    }

    /// 未配置时返回空，客户端据此沿用 v1 的 per-file CEK——
    /// 不能凭空造一把密钥，那会让新旧对象都解不开。
    #[test]
    fn no_configured_key_means_v2_is_off() {
        assert!(attachment_keys(&cfg(vec![])).is_empty());
        assert!(current_attachment_key(&cfg(vec![])).is_none());
    }

    /// 上传永远用**第一把**；其余是保留给老对象解密的。
    #[test]
    fn uploads_always_use_the_first_key() {
        let c = cfg(vec![
            (2, "current".into()),
            (1, "retired".into()),
        ]);
        let current = current_attachment_key(&c).expect("有当前密钥");
        assert_eq!(current.key_id, 2);
        assert_eq!(current.key, "current");
    }

    /// 下载给的是**集合**：轮换期两代对象并存，客户端按密文头里的 key_id 自己挑。
    /// 只给当前密钥的话，老对象会在轮换后立刻变成不可读。
    #[test]
    fn downloads_receive_every_retained_key() {
        let c = cfg(vec![(2, "current".into()), (1, "retired".into())]);
        let keys = attachment_keys(&c);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys.iter().map(|k| k.key_id).collect::<Vec<_>>(), vec![2, 1]);
    }
}
