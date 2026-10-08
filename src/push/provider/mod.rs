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

pub mod apns;
pub mod fcm; // ✨ Phase 2: FCM Provider
pub mod hms;
pub mod lenovo;
pub mod meizu;
pub mod mock;
pub mod oppo;
pub mod provider_trait; // ✨ Phase 3: APNs Provider
pub mod vivo;
pub mod xiaomi;
pub mod zte;

pub use apns::ApnsProvider;
pub use fcm::FcmProvider; // ✨ Phase 2
pub use hms::HmsProvider;
pub use lenovo::LenovoProvider;
pub use meizu::MeizuProvider;
pub use mock::MockProvider;
pub use oppo::OppoProvider;
pub use provider_trait::PushProvider; // ✨ Phase 3
pub use vivo::VivoProvider;
pub use xiaomi::XiaomiProvider;
pub use zte::ZteProvider;

/// 构造带超时的推送 HTTP client。
///
/// 🔴 绝不能用 `Client::new()`：reqwest 默认**无 connect timeout、无 request timeout**。
/// APNs/FCM 端点网络黑洞时（大陆机房到 FCM 是常态场景），每个请求永久挂起；配合
/// worker 的逐条 spawn，挂起任务只增不减，内存持续上涨且无法自愈——这正是
/// STABILITY_SPEC 禁令 2（禁无上限 spawn）与禁令 3（出站必须有超时）定义的事故形态。
///
/// connect 5s / 整体 15s：APNs HTTP/2 单请求正常 <1s，15s 足够兜住慢网络，又不至于
/// 让任务长期占用 worker 的并发 permit。超时后由 worker 外层统一记失败。
///
/// `build()` 只在 TLS 后端不可用时失败（rustls 已启用，属启动期灾难性配置错误），
/// 此时推送根本无法工作，fail-fast 比静默退回无超时 client 更安全。
pub fn build_http_client() -> reqwest::Client {
    build_http_client_with_proxy(None)
}

/// 可选出墙代理的推送 HTTP client。
///
/// 只有 FCM 需要它:生产服务器在墙内连不上 Google,经墙外中转。APNs/厂商推送传 `None`
/// 直连。代理 URL 解析失败时**不静默直连**(那会让人以为配了代理其实没走),直接 panic
/// 在启动期暴露配置错误——和私钥坏掉同样对待。
pub fn build_http_client_with_proxy(proxy: Option<&str>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15));
    if let Some(url) = proxy.map(str::trim).filter(|v| !v.is_empty()) {
        let p = reqwest::Proxy::all(url)
            .unwrap_or_else(|e| panic!("FCM 出墙代理 URL 无效 ({}): {}", url, e));
        builder = builder.proxy(p);
    }
    builder
        .build()
        .expect("构造推送 HTTP client 失败（TLS 后端不可用）")
}
