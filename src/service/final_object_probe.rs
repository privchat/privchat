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

//! final key 的对象探测（RESUMABLE_UPLOAD_SPEC §8.5）：HEAD / 流式回读 / 删除
//! 三个恢复原语。
//!
//! 🔴 这三个操作**不进 [`super::numbered_parts::NumberedPartBackend`]**（§8.7：
//! 该接口只管 MPU 控制操作）；生产实现委托现有存储层（OpenDAL Operator），随
//! `direct_upload` 门禁（实现顺序第 5 步）一起接入，接入前恒 `None`。
//!
//! 🔴 删除 final key 的唯一判据（§8.5 第 6 步）：对象 metadata
//! `privchat-upload-id == 当前 session_id` 才允许删。本接口只出原语，判据由
//! complete 分流在调用前核对——接口自己不猜归属。
//!
//! 🔴 **归属核对与删除必须是一个条件操作**（第十五轮评审 P1）：先 HEAD 验归属、
//! 再无条件 DELETE 存在 TOCTOU——两步之间 key 被替换时会删到别人的对象。因此
//! HEAD 结果携带 ETag，删除接口以 ETag 为条件（对应 S3 条件删除 / If-Match），
//! 不匹配即拒绝；调用方不得分两步完成安全判定。

use async_trait::async_trait;

// 校验入口按 final key 定位对象，用的就是这个引用；从这里再导出一次，调用方不必
// 为了一个坐标类型同时依赖 numbered_parts（MPU 控制面）。
pub use crate::service::numbered_parts::UploadReference;

/// HEAD 到的 final 对象：长度 + 归属 metadata + ETag。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalObjectHead {
    pub content_length: u64,
    /// 对象 metadata `privchat-upload-id`（CreateMultipartUpload 时写入，§2.2）。
    /// `None` = 对象没有该 metadata（无法证明归属 → 一律不得删除）。
    pub privchat_upload_id: Option<String>,
    /// 🔴 条件删除的凭据：删除时必须携带，ETag 不匹配即拒绝（防 TOCTOU）。
    pub etag: String,
}

/// 探测错误：只有后端错误一种，恢复语义由调用方按 §8.5 分流。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    Backend(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Backend(m) => write!(f, "{m}"),
        }
    }
}

/// final 对象的探测接口。定位只用 [`UploadReference`] 的 bucket/final_key
/// （provider_upload_id 与 final key 操作无关，但引用整体传递，调用方不必拆包）。
#[async_trait]
pub trait FinalObjectProbe: Send + Sync {
    /// HEAD final key：不存在回 `Ok(None)`；存在回长度与归属 metadata。
    async fn head(
        &self,
        reference: &UploadReference,
    ) -> Result<Option<FinalObjectHead>, ProbeError>;

    /// 单次流式 GET：交出**同一次响应**的 `Content-Length` 与字节流。
    ///
    /// 🔴 长度和字节必须来自同一次响应。拿之前 HEAD 的长度去核这次 GET 的字节，
    /// 核的是两个不同时刻的东西——两者之间对象可以被替换，而
    /// `verify_attachment` 把"长度已核过"当作后续 IO 失败一律可重试的前提。
    ///
    /// 🔴 也**不要**先 `sha256_of()` 再 GET 一次：`verify_attachment` 在同一趟里
    /// 同时算密文摘要和明文摘要，一次 GET 就够，两次等于把大对象回读两遍。
    ///
    /// S3 multipart 的 SHA-256 是 composite，不等于整文件摘要（FILE_STORAGE §3.5）：
    /// 文件身份的唯一权威就是这次回读之后的解密重算。
    async fn open_stream(
        &self,
        reference: &UploadReference,
    ) -> Result<(u64, std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>), ProbeError>;

    /// 条件删除 final 对象：🔴 只有当前对象的 ETag 与 `etag`（来自归属核对时
    /// 的 HEAD）一致才执行删除，把「核对」与「删除」合成一个原子判定，消除两步
    /// 之间对象被替换的 TOCTOU。调用方必须已核对归属（metadata == 当前
    /// session_id）。返回：`Ok(true)` 已删除；`Ok(false)` 对象已变化，拒绝删除
    /// （调用方回可重试错误，重试可自愈）；对象已不在视为 `Ok(true)`（幂等）。
    async fn delete_if_match(
        &self,
        reference: &UploadReference,
        etag: &str,
    ) -> Result<bool, ProbeError>;
}
