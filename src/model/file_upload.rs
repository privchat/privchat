//! 文件上传相关模型（与 repository、service 共用，避免循环依赖）

use serde::{Deserialize, Serialize};

/// 文件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileType {
    Image,
    Video,
    Audio,
    File,
    Other,
}

impl FileType {
    pub fn as_str(&self) -> &str {
        match self {
            FileType::Image => "image",
            FileType::Video => "video",
            FileType::Audio => "audio",
            FileType::File => "file",
            FileType::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "image" => Some(FileType::Image),
            "video" => Some(FileType::Video),
            "audio" => Some(FileType::Audio),
            "file" => Some(FileType::File),
            "other" => Some(FileType::Other),
            _ => None,
        }
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
}
