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

use async_trait::async_trait;

use crate::service::numbered_parts::UploadReference;

/// HEAD 到的 final 对象：长度 + 归属 metadata。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalObjectHead {
    pub content_length: u64,
    /// 对象 metadata `privchat-upload-id`（CreateMultipartUpload 时写入，§2.2）。
    /// `None` = 对象没有该 metadata（无法证明归属 → 一律不得删除）。
    pub privchat_upload_id: Option<String>,
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

    /// 流式回读 final 对象，算**整文件** SHA-256（hex 小写）。
    /// 🔴 S3 multipart 的 SHA-256 是 composite，不等于整文件摘要（FILE_STORAGE
    /// §3.5）：文件身份的唯一权威就是这次回读。
    async fn sha256_of(&self, reference: &UploadReference) -> Result<String, ProbeError>;

    /// 删除 final 对象。🔴 调用方必须已核对归属（metadata == 当前 session_id）；
    /// 幂等：对象已不在视为成功。
    async fn delete(&self, reference: &UploadReference) -> Result<(), ProbeError>;
}
