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

//! 文件上传端到端测试示例
//!
//! 测试流程：
//! 1. 连接到服务器并认证
//! 2. 请求上传 token (RPC: file/request_upload_token)
//! 3. 上传文件 (HTTP: POST /api/v1/files/upload)
//! 4. 发送图片消息（携带 file_id）
//! 5. 验证接收方能收到文件消息

use privchat_server::config::ServerConfig;
use privchat_server::server::ChatServer;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 启动文件上传端到端测试");

    // 1. 启动服务器
    info!("📡 启动服务器...");
    let config = ServerConfig::default();
    let server = Arc::new(ChatServer::new(config).await?);
    let server_clone = server.clone();

    tokio::spawn(async move {
        if let Err(e) = server_clone.run().await {
            error!("❌ 服务器运行失败: {}", e);
        }
    });

    // 等待服务器启动
    sleep(Duration::from_secs(2)).await;
    info!("✅ 服务器已启动");

    // 2. 模拟文件上传流程
    info!("\n📤 开始测试文件上传流程...\n");

    // TODO: 实现客户端连接和 RPC 调用
    // 由于当前没有完整的客户端 SDK，这里展示完整的测试流程说明

    info!("📋 完整测试流程说明：");
    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    info!("\n步骤 1️⃣: 请求上传 token");
    info!("  RPC 调用: file/request_upload_token");
    info!("  请求参数:");
    info!(
        r#"  {{
    "user_id": "alice",
    "file_type": "image",
    "file_size": 102400,
    "mime_type": "image/jpeg",
    "business_type": "message",
    "filename": "photo.jpg"
  }}"#
    );
    info!("  预期响应:");
    info!(
        r#"  {{
    "upload_token": "uuid-token-here",
    "upload_url": "http://localhost:8083/api/v1/files/upload",
    "expires_at": 1234567890,
    "max_size": 10485760
  }}"#
    );

    info!("\n步骤 2️⃣: 上传文件");
    info!("  HTTP 请求: POST http://localhost:8083/api/v1/files/upload");
    info!("  Headers:");
    info!("    X-Upload-Token: <token from step 1>");
    info!("    Content-Type: multipart/form-data");
    info!("  Form Data:");
    info!("    file: <binary data>");
    info!("    compress: true");
    info!("    generate_thumbnail: true");
    info!("  预期响应:");
    info!(
        r#"  {{
    "file_id": "file_abc123",
    "file_url": "http://localhost:8083/files/images/file_abc123.jpg",
    "thumbnail_url": "http://localhost:8083/files/thumbnails/file_abc123.jpg",
    "file_size": 102400,
    "width": 1920,
    "height": 1080,
    "mime_type": "image/jpeg"
  }}"#
    );

    info!("\n步骤 3️⃣: 发送图片消息");
    info!("  RPC 调用: SendMessageRequest");
    info!("  消息 payload:");
    info!(
        r#"  {{
    "content": "[图片]",
    "metadata": {{
      "image": {{
        "file_id": "file_abc123",
        "width": 1920,
        "height": 1080,
        "thumbnail_url": "http://localhost:8083/files/thumbnails/file_abc123.jpg",
        "file_size": 102400,
        "mime_type": "image/jpeg"
      }}
    }}
  }}"#
    );

    info!("\n步骤 4️⃣: 接收方获取消息");
    info!("  接收方通过 PushMessageRequest 接收到消息");
    info!("  解析 metadata.image.file_id");
    info!("  调用 RPC: file/get_file_url 获取文件访问 URL");

    info!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // 3. 使用 curl 测试示例
    info!("\n🔧 使用 curl 测试的完整命令：\n");

    info!("# 1. 请求上传 token (RPC)");
    info!("# 注意：需要通过 WebSocket 或 TCP 连接发送 RPC 请求");
    info!("# 这里展示 payload 格式，实际需要封装成 privchat 协议");

    info!("\n# 2. 上传文件 (HTTP)");
    info!(
        r#"curl -X POST http://localhost:8083/api/v1/files/upload \
  -H "X-Upload-Token: YOUR_TOKEN_HERE" \
  -F "file=@/path/to/photo.jpg" \
  -F "compress=true" \
  -F "generate_thumbnail=true""#
    );

    info!("\n# 3. 获取文件 URL (HTTP)");
    info!(r#"curl http://localhost:8083/api/v1/files/FILE_ID/url?user_id=alice"#);

    info!("\n# 4. 下载文件 (HTTP)");
    info!(r#"curl http://localhost:8083/files/FILE_ID -o downloaded.jpg"#);

    info!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

    // 4. 实际测试建议
    info!("\n💡 实际测试建议：");
    info!("  1. 运行服务器：cargo run --example simple_server_demo");
    info!("  2. 使用 SDK 客户端连接并调用 file/request_upload_token RPC");
    info!("  3. 使用 curl 或 Postman 上传文件");
    info!("  4. 使用 SDK 客户端发送图片消息");
    info!("  5. 验证接收方能收到消息并获取文件");

    info!("\n✅ 测试流程说明完成");
    info!("💡 提示：完整的自动化测试需要等待 SDK 支持文件相关 RPC");

    // 保持服务器运行
    info!("\n🔄 服务器持续运行中，按 Ctrl+C 退出...");
    tokio::signal::ctrl_c().await?;

    info!("👋 服务器关闭");
    Ok(())
}
