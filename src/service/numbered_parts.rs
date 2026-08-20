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

//! S3 直传的分片后端控制接口（RESUMABLE_UPLOAD_SPEC §8.7、FILE_STORAGE_SPEC §3.5）。
//!
//! 🔴 要兼容的是两种 HTTP 分片上传数据面，与 OpenDAL 的 Multipart 能力无直接关系：
//! S3 控制操作只定义内部接口（`NumberedPartBackend`），实现不绑定具体库；HEAD / 流式
//! 回读继续用现有存储层，不进该接口。具体后端实现随 `direct_upload` 门禁（实现顺序
//! 第 5 步）一起接入，接入前本模块只提供接口与纯函数。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 预签名 URL 有效期（RESUMABLE §8.3）：15 分钟，过期重拉，不影响已传分片。
pub const PART_URL_TTL_SECS: u64 = 15 * 60;

/// 单次 `POST /files/part-url` 的批量上限。
pub const MAX_PARTS_PER_REQUEST: usize = 100;

/// S3 控制面错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumberedPartError {
    /// MPU 已被关闭（abort / complete / 过期）。🔴 它**不是**归属证明
    /// （RESUMABLE §2.2）：调用方按自己的恢复分支处理，不得据此删对象。
    NoSuchUpload,
    /// `CompleteMultipartUpload` 带 `If-None-Match: *` 收到 **409**：MPU 作废，
    /// 调用方回 20618 RestartUpload，客户端废弃会话从零重来（RESUMABLE §8.5）。
    /// 🔴 与 [`NumberedPartError::PreconditionFailed`] 不得混同：那是「目标已存在、
    /// 可核验复用」，这个是「本次上传作废」。
    Conflict,
    /// `CompleteMultipartUpload` 带 `If-None-Match: *` 收到 **412**：final key 上已有
    /// 对象、本次 MPU 未发布。🔴 绝不删除 final key：调用方回读已有对象核验身份
    /// （privchat-upload-id）——一致则复用（先幂等 abort 当前 MPU 再建行）；不一致
    /// 则保留已有对象、abort 当前 MPU、报内部冲突（RESUMABLE §8.5）。
    PreconditionFailed,
    /// 其余后端错误：可重试与否由调用方按 HTTP 语义决定。
    Backend(String),
}

/// 一次进行中 MPU 的**可持久化**引用（RESUMABLE §8.7）：bucket/final_key/uploadId
/// 三件套与 manifest 的三个平铺字段一一对应（作为整体原子使用），进程重启后
/// 照常恢复。🔴 `provider_upload_id` 冻结为 provider 的原始 UploadId——不得把
/// bucket/final_key 编进去，也不得靠进程内映射反查（重启即失忆，续传就断了）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadReference {
    pub bucket: String,
    /// 最终对象 key（spec 字段名 `final_key`）：建 MPU 前由调用方定死，之后不可改。
    pub final_key: String,
    pub provider_upload_id: String,
}

/// `ListParts` 换算用的一片（status 分支把它转成现有区间格式）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPart {
    pub part_number: u32,
    pub size: u64,
    pub etag: String,
    /// 该片在 S3 侧记录的 SHA-256（Base64）。创建时声明了
    /// `ChecksumAlgorithm=SHA256` 就应该有；`None` = 后端没回，调用方按缺失处理。
    pub checksum_sha256_b64: Option<String>,
}

/// `CompleteMultipartUpload` 的一片。🔴 spec 冻结 part_number / ETag /
/// ChecksumSHA256 **三字段缺一不可**（会话声明了 SHA256，Complete 必须携带每片
/// checksum，RESUMABLE §8.5）：所以 checksum 是 `String` 而非 `Option`，不变量由
/// 类型保证——缺摘要的片根本构造不出来，绝无可能被提交给 Complete。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedPart {
    pub part_number: u32,
    pub etag: String,
    /// 与 part-url 签发时同源的片 checksum（RFC 4648 标准 Base64）。
    pub checksum_sha256_b64: String,
}

/// 分片号从 1 起、字节直连 S3 的控制接口。实现不绑定具体库（§8.7）。
///
/// 🔴 所有方法都以 [`UploadReference`] 定位 MPU（bucket/key/uploadId 三件套）：
/// 它整体持久化在 manifest，任何实现都不得依赖进程内状态。
#[async_trait]
pub trait NumberedPartBackend: Send + Sync {
    /// 创建 multipart upload，返回完整 [`UploadReference`]（存入 manifest 的
    /// bucket/final_key/provider_upload_id 三个平铺字段）。`bucket`/`final_key`
    /// 由调用方按存储路径规则预先定死——MPU 之后对象坐标不可再改。
    /// 🔴 创建时必须写对象 metadata `privchat-upload-id = {session_id}` 并声明
    /// `ChecksumAlgorithm=SHA256`（RESUMABLE §2.2）：前者是最终对象归属的唯一
    /// 证明（HEAD 恢复时核对），后者让每片 checksum 可查可验。
    async fn create(
        &self,
        session_upload_id: &str,
        bucket: &str,
        final_key: &str,
        total_size: u64,
    ) -> Result<UploadReference, NumberedPartError>;

    /// 预签名单片 UploadPart URL（有效 `ttl_secs` 秒）。🔴 `checksum_sha256_b64`
    /// 已是 RFC 4648 标准 Base64（见 [`checksum_b64_from_hex`]），后端把它签进
    /// `x-amz-checksum-sha256`；`Content-Length` 不签（浏览器不允许手动设置）。
    async fn sign_part_url(
        &self,
        reference: &UploadReference,
        part_number: u32,
        content_length: u64,
        checksum_sha256_b64: &str,
        ttl_secs: u64,
    ) -> Result<String, NumberedPartError>;

    /// 已传分片列表（status 分支换算区间用；调用方逐片校验长度，异常视为缺失）。
    async fn list_parts(
        &self,
        reference: &UploadReference,
    ) -> Result<Vec<ListedPart>, NumberedPartError>;

    /// 组装分片，🔴 **必须携带 `If-None-Match: *`**（最终 key 的 no-clobber 是存储层
    /// 职责，RESUMABLE §8.5）：`409` → MPU 作废，回 [`NumberedPartError::Conflict`]；
    /// `412` → final key 已存在、本次未发布，回 [`NumberedPartError::PreconditionFailed`]，
    /// 二者不得混同（恢复路径完全相反）。
    async fn complete(
        &self,
        reference: &UploadReference,
        parts: &[CompletedPart],
    ) -> Result<(), NumberedPartError>;

    /// 中止 MPU：🔴 幂等，`NoSuchUpload` 视为成功。
    async fn abort(&self, reference: &UploadReference) -> Result<(), NumberedPartError>;
}

/// complete 失败后的恢复动作（RESUMABLE §8.5）。
///
/// 🔴 这是冻结语义的唯一真源：409 与 412 的恢复路径**完全相反**，第 3 步 complete
/// 分流必须调本函数拿动作，不得自行按 HTTP 码猜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteRecovery {
    /// 409：MPU 作废 → 回 20618 RestartUpload，客户端废弃会话从零重来。
    RestartUpload,
    /// 412：final key 上已有对象、本次 MPU 未发布 → 回读已有对象核验身份：
    /// 一致复用（先幂等 abort 当前 MPU 再建行），不一致保留已有对象、abort
    /// 当前 MPU、报内部冲突。🔴 绝不删除 final key。
    VerifyExistingObject,
}

/// complete 分流对照：后端错误 → 恢复动作。`NoSuchUpload`/`Backend` 不在 complete
/// 的两个冻结分支里，回 `None` 由调用方按自己的恢复链处理（如 HEAD final_key）。
pub fn complete_recovery_for(e: &NumberedPartError) -> Option<CompleteRecovery> {
    match e {
        NumberedPartError::Conflict => Some(CompleteRecovery::RestartUpload),
        NumberedPartError::PreconditionFailed => Some(CompleteRecovery::VerifyExistingObject),
        NumberedPartError::NoSuchUpload | NumberedPartError::Backend(_) => None,
    }
}

/// 🔴 checksum 编码冻结（RESUMABLE §8.3）：客户端沿用 `X-Chunk-SHA256` 口径传
/// **64 位十六进制**；服务端 hex 解码 32 字节 → **RFC 4648 标准 Base64（保留 `=`
/// padding，禁止 base64url）** → 签入 `x-amz-checksum-sha256`。禁止把 hex 直接填进
/// S3 header——填 hex 会让所有 UploadPart 失败。
pub fn checksum_b64_from_hex(hex_str: &str) -> Result<String, String> {
    let s = hex_str.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("checksum_sha256_hex 必须是 64 位十六进制".to_string());
    }
    let bytes =
        hex::decode(s).map_err(|e| format!("checksum_sha256_hex 解码失败: {e}"))?;
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// 单片几何校验（与 chunk 端点口径一致，RESUMABLE §8.3）：
/// `part_number ∈ [1, total_parts]`、非末片长度 = `part_size`、末片 = 余数。
pub fn check_part_geometry(
    part_number: u32,
    content_length: u64,
    total_parts: u32,
    part_size: u64,
    total_size: u64,
) -> Result<(), String> {
    if part_number == 0 || part_number > total_parts {
        return Err(format!(
            "part_number 必须在 [1, {total_parts}]，收到 {part_number}"
        ));
    }
    let expected = if part_number == total_parts {
        // 末片 = 余数；整除时末片就是一个完整 part_size。
        let rem = total_size % part_size;
        if rem == 0 {
            part_size
        } else {
            rem
        }
    } else {
        part_size
    };
    if content_length != expected {
        return Err(format!(
            "part {part_number} 长度必须是 {expected}，收到 {content_length}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_encoding_is_rfc4648_standard_base64_with_padding() {
        // 全零 32 字节：Base64 恰好带 `==` padding，能盯住「保留 padding、
        // 非 base64url」两点。
        let hex64 = "0".repeat(64);
        let b64 = checksum_b64_from_hex(&hex64).unwrap();
        assert_eq!(b64, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=");
        // 已知向量：0x01 * 32。
        let hex01 = "01".repeat(32);
        let b64 = checksum_b64_from_hex(&hex01).unwrap();
        use base64::Engine as _;
        assert_eq!(
            b64,
            base64::engine::general_purpose::STANDARD.encode([1u8; 32])
        );
    }

    #[test]
    fn checksum_encoding_rejects_bad_input() {
        assert!(checksum_b64_from_hex("").is_err());
        assert!(checksum_b64_from_hex(&"0".repeat(63)).is_err());
        assert!(checksum_b64_from_hex(&"0".repeat(65)).is_err());
        let mut bad = "0".repeat(64);
        bad.replace_range(0..1, "g"); // 非 hex 字符
        assert!(checksum_b64_from_hex(&bad).is_err());
    }

    #[test]
    fn part_geometry_follows_the_chunk_endpoint_rules() {
        // 10 MiB 文件、4 MiB 片：3 片，末片 2 MiB。
        let (total, size) = (10u64 << 20, 4u64 << 20);
        assert!(check_part_geometry(1, size, 3, size, total).is_ok());
        assert!(check_part_geometry(2, size, 3, size, total).is_ok());
        assert!(check_part_geometry(3, 2 << 20, 3, size, total).is_ok());
        // 越界 / 长度不符。
        assert!(check_part_geometry(0, size, 3, size, total).is_err());
        assert!(check_part_geometry(4, size, 3, size, total).is_err());
        assert!(check_part_geometry(1, size - 1, 3, size, total).is_err());
        assert!(check_part_geometry(3, size, 3, size, total).is_err());
        // 整除：末片就是完整 part_size。
        let (total, size) = (8u64 << 20, 4u64 << 20);
        assert!(check_part_geometry(2, size, 2, size, total).is_ok());
    }

    /// 🔴 complete 分流对照（RESUMABLE §8.5，第十三轮评审 P0）：
    /// 409（Conflict）→ MPU 作废 → 20618 重启；412（PreconditionFailed）→
    /// final key 已存在 → 核验复用。两者恢复路径完全相反，写反即事故。
    #[test]
    fn complete_recovery_maps_409_to_restart_and_412_to_verify() {
        assert_eq!(
            complete_recovery_for(&NumberedPartError::Conflict),
            Some(CompleteRecovery::RestartUpload),
            "409 = MPU 作废：回 20618，客户端从零重来"
        );
        assert_eq!(
            complete_recovery_for(&NumberedPartError::PreconditionFailed),
            Some(CompleteRecovery::VerifyExistingObject),
            "412 = final key 已存在：回读核验复用，绝不删除"
        );
        // 两条分支不得互串：这正是写反语义的回归守卫。
        assert_ne!(
            complete_recovery_for(&NumberedPartError::Conflict),
            complete_recovery_for(&NumberedPartError::PreconditionFailed)
        );
        // NoSuchUpload / Backend 不属于这两个冻结分支。
        assert_eq!(complete_recovery_for(&NumberedPartError::NoSuchUpload), None);
        assert_eq!(
            complete_recovery_for(&NumberedPartError::Backend("x".into())),
            None
        );
    }
}
