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

//! 文件上传相关模型（与 repository、service 共用，避免循环依赖）

use serde::{Deserialize, Serialize};

/// 文件类型（存储层分类）
///
/// 与消息类型分层：Image / Video / Voice 各自有独立的尺寸限制、存储目录、
/// 清理策略（比如语音通常远小于视频，单独限额）；其它文件（含普通音频 mp3/wav 等）
/// 一律归入 File。Voice 在存储层独立，是因为业务侧需要按类别做额度管理，
/// 不是为了决定消息类型 —— 消息类型依旧由发送入口 `ContentMessageType` 决定。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileType {
    Image,
    Video,
    Voice,
    File,
    Other,
}

impl FileType {
    pub fn as_str(&self) -> &str {
        match self {
            FileType::Image => "image",
            FileType::Video => "video",
            FileType::Voice => "voice",
            FileType::File => "file",
            FileType::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "image" => Some(FileType::Image),
            "video" => Some(FileType::Video),
            "voice" => Some(FileType::Voice),
            "file" => Some(FileType::File),
            "other" => Some(FileType::Other),
            _ => None,
        }
    }

    /// 单个文件的大小硬顶。**这是唯一的一份**。
    ///
    /// 曾经有三份互相不一致的限额：签发 token 时按一套（视频 200MB），axum body limit
    /// 一套（120MB），流式写入时又一套（视频 100MB）。把关的永远是最松的那个，于是一个
    /// 150MB 的视频能拿到 token，客户端老老实实传了几分钟，再被写入侧掐断——用户等了很久
    /// 才失败，而这本可以在第一秒就拒绝。
    ///
    /// 所以：签发 token 和写入校验必须读同一个函数，谁都不许再写自己的表。
    pub const fn max_size_bytes(&self) -> u64 {
        const MB: u64 = 1024 * 1024;
        match self {
            FileType::Image => 10 * MB,
            FileType::Video => 100 * MB,
            FileType::Voice => 10 * MB,
            FileType::File => 50 * MB,
            FileType::Other => 10 * MB,
        }
    }

    /// 所有类型里最大的那个硬顶，用于推导 HTTP body limit。
    pub const fn max_size_bytes_any() -> u64 {
        let mut max = FileType::Image.max_size_bytes();
        // const fn 里不能用迭代器，逐个比。新增类型时编译器不会提醒，所以下面有测试兜底。
        if FileType::Video.max_size_bytes() > max {
            max = FileType::Video.max_size_bytes();
        }
        if FileType::Voice.max_size_bytes() > max {
            max = FileType::Voice.max_size_bytes();
        }
        if FileType::File.max_size_bytes() > max {
            max = FileType::File.max_size_bytes();
        }
        if FileType::Other.max_size_bytes() > max {
            max = FileType::Other.max_size_bytes();
        }
        max
    }

    /// HTTP body 的上限：最大硬顶再留一点余量。
    ///
    /// body 里除了文件本体还有 multipart 的边界、字段名、以及附件加密 v1 的
    /// `encryption_version` / `cek` 两个文本字段。余量必须留够，否则 multipart 会在
    /// 业务校验跑起来之前就被 axum 拒掉，用户拿到的是一个没有业务含义的 413。
    pub const fn http_body_limit_bytes() -> usize {
        (Self::max_size_bytes_any() + 4 * 1024 * 1024) as usize
    }
}

/// 文件上传记录元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// 文件ID（数据库 BIGSERIAL 自增，u64）
    pub file_id: u64,
    pub original_filename: String,
    pub file_size: u64,
    pub original_size: Option<u64>,
    pub file_type: FileType,
    pub mime_type: String,
    /// 存储路径（如 public/chat/message/202601/{file_hash}），与 storage_source_id 配合定位文件
    pub file_path: String,
    /// 存储源：0=本地，1=S3，2=阿里云 OSS，3=腾讯云 COS 等
    pub storage_source_id: u32,
    pub uploader_id: u64,
    /// 上传时客户端 IP（便于审计与安全，可选）
    pub uploader_ip: Option<String>,
    /// 上传时间（毫秒时间戳，u64）
    pub uploaded_at: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub file_hash: Option<String>,
    /// 业务类型（如 message/avatar/group_avatar），便于按业务清理
    pub business_type: Option<String>,
    /// 业务具体ID（字符串，兼容各类业务如 message_id/uuid 等），便于随业务数据删除时清理
    pub business_id: Option<String>,
    /// 附件加密版本：0=明文 legacy；1=AES-256-GCM（客户端加密，见 ATTACHMENT_ENCRYPTION_SPEC）
    pub encryption_version: i32,
    /// 内容密钥 CEK：base64url(no-pad) 的 32 字节；nonce 在密文 blob 头部，不入库。
    /// 仅在鉴权后的 get_url 响应返回，绝不进日志/URL。version=0 时为 None。
    pub cek: Option<String>,
    /// v2：本文件用的是哪一把全站密钥（对应 config `[[attachment.keys]].id`
    /// 与密文 blob 头部的 key_id）。`None` = 非 v2。
    ///
    /// 记在行上，密钥轮换才不影响存量对象：`get_url` 按它取出**这一把**返回，
    /// 既不必重新加密，也不必把全部历史密钥一起下发。
    pub encryption_key_id: Option<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [FileType; 5] = [
        FileType::Image,
        FileType::Video,
        FileType::Voice,
        FileType::File,
        FileType::Other,
    ];

    /// 签发 token 用的限额，必须**就是**写入时掐断用的限额。
    ///
    /// 这两处曾经分家（视频 200MB vs 100MB），后果不是「拒绝得不够严」，而是
    /// 「先放进来再掐断」：客户端拿到 token，把 150MB 传了几分钟，才在写入侧失败。
    #[test]
    fn issuing_and_enforcing_read_the_same_limit() {
        for ft in ALL {
            let issued = crate::rpc::file::request_upload_token::max_size_for_type_for_tests(&ft);
            assert_eq!(
                issued as u64,
                ft.max_size_bytes(),
                "{:?}: 签发限额与写入硬顶必须同源",
                ft
            );
        }
    }

    /// body limit 必须高于任何一个业务硬顶。
    ///
    /// 反过来的话，超限文件会在 multipart 解析阶段就被 axum 打回一个 413，
    /// 业务校验根本没机会跑，客户端拿到的错误里没有「哪个类型、超了多少」。
    #[test]
    fn the_body_limit_leaves_room_above_every_business_cap() {
        for ft in ALL {
            assert!(
                (FileType::http_body_limit_bytes() as u64) > ft.max_size_bytes(),
                "{:?}: body limit 必须高于业务硬顶",
                ft
            );
        }
    }

    /// `max_size_bytes_any` 是手写展开的（const fn 里没有迭代器），新增类型时
    /// 编译器不会提醒。这条测试就是那个提醒。
    #[test]
    fn the_hand_rolled_max_actually_covers_every_type() {
        let expected = ALL.iter().map(|ft| ft.max_size_bytes()).max().unwrap();
        assert_eq!(FileType::max_size_bytes_any(), expected);
    }
}
