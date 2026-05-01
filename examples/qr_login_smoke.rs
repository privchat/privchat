// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Web/PC unauth RPC 扫码登录联调用 smoke 工具（spec QR_API §5）。
//!
//! 功能：
//! 1. 用 `msgtrans::TransportClient` 连一条 unauth WebSocket 到 privchat-server
//! 2. 发 RPC `qr_login/create_scene`，打印返回的 `scene_id` / `qr_token` / `expires_at`
//! 3. 持续监听服务端 `PushMessageRequest`（biz_type=7），把 `qr_login.*` 事件解码 + 打印
//! 4. Ctrl-C 退出
//!
//! 用途：QR Login E2E B3 联调时，运营 / 测试同学不需要造完整 Web UI，跑这个 binary
//! 就能模拟 Web/PC 一侧。配合 application 侧 `/platform/qr-login/scan|confirm|reject`
//! HTTP（curl 一下）就能跑全链路。
//!
//! ## 运行
//!
//! ```bash
//! # 默认连本机 ws://127.0.0.1:9080/gate（与 config.toml 默认 listener 一致）
//! cargo run --example qr_login_smoke
//!
//! # 指定其他地址 / TTL
//! cargo run --example qr_login_smoke -- \
//!     --url ws://127.0.0.1:9080/gate \
//!     --ttl-secs 90
//! ```
//!
//! ## 输出示例
//!
//! ```text
//! [smoke] connecting to ws://127.0.0.1:9080/gate
//! [smoke] connected, sending qr_login/create_scene
//! [smoke] scene created:
//!   scene_id   = 9f3b...
//!   qr_token   = qr_xxx
//!   expires_at = 1745632890000
//!   rpc_topic  = qr_login.scene.9f3b...
//! [smoke] listening for push events (Ctrl-C to quit)...
//! [smoke] event=qr_login.scanned state=scanned data={"scanner_uid":100,...}
//! [smoke] event=qr_login.authorized state=authorized data={"userId":100,"accessToken":"...",...}
//! ```

use std::time::Duration;

use bytes::Bytes;
use msgtrans::protocol::WebSocketClientConfig;
use msgtrans::transport::client::TransportClientBuilder;
use msgtrans::transport::TransportOptions;
use msgtrans::ClientEvent;
use privchat_protocol::protocol::{MessageType, RpcRequest, RpcResponse};
use privchat_protocol::rpc::qr_login::{
    QrLoginCreateSceneRequest, QrLoginCreateSceneResponse, QrLoginPushEvent,
};
use privchat_protocol::rpc::routes;
use privchat_protocol::decode_message;
use serde_json::Value;

#[derive(Debug)]
struct Args {
    url: String,
    ttl_secs: i64,
    web_device_id: String,
}

fn parse_args() -> Args {
    let mut url = "ws://127.0.0.1:9080/gate".to_string();
    let mut ttl_secs: i64 = 90;
    let mut web_device_id = format!("smoke-{}", uuid::Uuid::new_v4());

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--url" => {
                if let Some(v) = iter.next() {
                    url = v;
                }
            }
            "--ttl-secs" => {
                if let Some(v) = iter.next() {
                    ttl_secs = v.parse().unwrap_or(90);
                }
            }
            "--device-id" => {
                if let Some(v) = iter.next() {
                    web_device_id = v;
                }
            }
            "-h" | "--help" => {
                eprintln!(
                    "qr_login_smoke — QR Login unauth RPC smoke client\n\
                     \n\
                     USAGE:\n  \
                     cargo run --example qr_login_smoke [-- ARGS]\n\
                     \n\
                     ARGS:\n  \
                     --url URL          unauth WebSocket URL (default: ws://127.0.0.1:9080/gate)\n  \
                     --ttl-secs N       scene TTL in seconds (default: 90)\n  \
                     --device-id ID     simulated Web device id (default: random uuid)"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("[smoke] unknown arg: {}", other);
                std::process::exit(2);
            }
        }
    }

    Args {
        url,
        ttl_secs,
        web_device_id,
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();

    eprintln!("[smoke] connecting to {}", args.url);

    let cfg = WebSocketClientConfig::new(&args.url)?
        .with_connect_timeout(Duration::from_secs(10))
        .with_verify_tls(false);
    let mut client = TransportClientBuilder::new()
        .with_protocol(cfg)
        .connect_timeout(Duration::from_secs(10))
        .build()
        .await?;
    client.connect().await?;

    eprintln!("[smoke] connected, sending qr_login/create_scene");

    // 发 RPC（protocol crate `RpcRequest` 和 server `RPCMessageRequest` 是同一 JSON 形态）
    let rpc_req = RpcRequest {
        route: routes::qr_login::CREATE_SCENE.to_string(),
        body: serde_json::to_value(QrLoginCreateSceneRequest {
            purpose: "login".to_string(),
            web_device_id: args.web_device_id.clone(),
            web_device_info: None,
            ttl_secs: Some(args.ttl_secs),
        })?,
    };
    let req_bytes = serde_json::to_vec(&rpc_req)?;

    // events 必须在 connect() 之后、request() 之前订阅，避免漏掉早到的 push
    let mut events = client.subscribe_events();

    let opt = TransportOptions::new()
        .with_biz_type(MessageType::RpcRequest as u8)
        .with_timeout(Duration::from_secs(15));
    let raw = client
        .request_with_options(Bytes::from(req_bytes), opt)
        .await?;

    let resp: RpcResponse = serde_json::from_slice(&raw)?;
    if resp.code != 0 {
        eprintln!(
            "[smoke] create_scene failed: code={} message={}",
            resp.code, resp.message
        );
        return Err(format!("create_scene failed (code={})", resp.code).into());
    }
    let data = resp
        .data
        .ok_or("create_scene response.data is empty")?;
    let scene: QrLoginCreateSceneResponse = serde_json::from_value(data)?;

    eprintln!("[smoke] scene created:");
    eprintln!("  scene_id   = {}", scene.scene_id);
    eprintln!("  qr_token   = {}", scene.qr_token);
    eprintln!("  expires_at = {}", scene.expires_at);
    eprintln!("  rpc_topic  = {}", scene.rpc_topic);
    eprintln!();
    eprintln!("[smoke] now use admin HTTP to drive the scene from the App side, e.g.:");
    eprintln!(
        "  curl -X POST 'http://127.0.0.1:9090/api/service/qr-login/scenes/{}/scan' \\
       -H 'X-Service-Key: <SERVICE_MASTER_KEY>' -H 'Content-Type: application/json' \\
       -d '{{\"scanner_uid\":100, \"scanner_device_id\":\"ios-1\", \"qr_token\":\"{}\"}}'",
        scene.scene_id, scene.qr_token
    );
    eprintln!();
    eprintln!("[smoke] listening for push events (Ctrl-C to quit)...");

    // 监听 push 事件
    let push_biz = MessageType::PushMessageRequest as u8;
    let scene_id = scene.scene_id.clone();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\n[smoke] interrupted, disconnecting");
                let _ = client.disconnect().await;
                return Ok(());
            }
            ev = events.recv() => {
                let Ok(ev) = ev else { continue };
                match ev {
                    ClientEvent::Connected { info } => {
                        eprintln!("[smoke] connected info={:?}", info.peer_addr);
                    }
                    ClientEvent::Disconnected { reason } => {
                        eprintln!("[smoke] disconnected: {:?}", reason);
                        return Ok(());
                    }
                    ClientEvent::MessageReceived(ctx) => {
                        if ctx.biz_type != push_biz {
                            continue;
                        }
                        let Ok(push) = decode_message::<privchat_protocol::protocol::PushMessageRequest>(&ctx.data) else {
                            eprintln!("[smoke] PushMessageRequest decode failed");
                            continue;
                        };
                        if !push.topic.starts_with("qr_login.") {
                            // 其他 IM push 略过（unauth 连接不太可能收到，但保险起见）
                            continue;
                        }
                        // payload = 我们 serde_json::to_vec(QrLoginPushEvent) 写进去的字节
                        match serde_json::from_slice::<QrLoginPushEvent>(&push.payload) {
                            Ok(qr_event) => {
                                let data_preview = qr_event
                                    .data
                                    .as_ref()
                                    .map(|v: &Value| v.to_string())
                                    .unwrap_or_else(|| "null".to_string());
                                eprintln!(
                                    "[smoke] event={} scene_id={} state={} data={}",
                                    qr_event.event,
                                    qr_event.scene_id,
                                    qr_event.state,
                                    data_preview
                                );
                                // 终态自动退出
                                let terminal = matches!(
                                    qr_event.event.as_str(),
                                    "qr_login.authorized" | "qr_login.rejected" | "qr_login.expired"
                                );
                                if terminal && qr_event.scene_id == scene_id {
                                    eprintln!("[smoke] terminal event received, exiting");
                                    let _ = client.disconnect().await;
                                    return Ok(());
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "[smoke] qr_login push payload decode failed: {} topic={}",
                                    e, push.topic
                                );
                            }
                        }
                    }
                    ClientEvent::MessageSent { .. } => {}
                    ClientEvent::Error { error } => {
                        eprintln!("[smoke] transport error: {}", error);
                    }
                }
            }
        }
    }
}
