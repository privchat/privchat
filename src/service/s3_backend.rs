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

//! S3 直传的生产分片后端与对象探测（RESUMABLE_UPLOAD_SPEC §8.7 / §8.5，
//! 第十六轮评审 P0：真实链路接线）。
//!
//! 🔴 实现不绑定具体库（§8.7）：直接对 S3 REST API 发 SigV4 签名请求
//! （`reqsign`——OpenDAL 的 S3 服务本来就靠它签名，零新增传递依赖），
//! 因为只有裸 REST 才能表达冻结语义要求的全部控制位：
//! `CreateMultipartUpload` 的对象 metadata + `ChecksumAlgorithm=SHA256`、
//! UploadPart 预签名把 `x-amz-checksum-sha256` 签进 URL、`CompleteMultipartUpload`
//! 携带 `If-None-Match: *` 与逐片 checksum、`DeleteObject` 携带 `If-Match`
//! 的条件删除（§8.5 归属核对与删除合成一个原子判定）。
//!
//! 🔴 寻址方式：配置了 `endpoint`（MinIO/Garage/OSS/COS 等）一律 path-style
//! `{endpoint}/{bucket}/{key}`——自建后端普遍不支持虚拟主机寻址。

use std::time::Duration;

use async_trait::async_trait;
use reqwest::StatusCode;

use crate::config::FileStorageSourceConfig;
use crate::error::ServerError;
use crate::service::final_object_probe::{FinalObjectHead, FinalObjectProbe, ProbeError};
use crate::service::numbered_parts::{
    CompletedPart, ListedPart, NumberedPartBackend, NumberedPartError, UploadReference,
};

/// `direct_upload` 显式开关的唯一合法值（RESUMABLE §8.2）。
pub const DIRECT_UPLOAD_S3_MULTIPART_V1: &str = "s3_multipart_v1";

/// 启动期能力探测专用 key 前缀（第十八轮评审）：探测只在该前缀下建临时对象，
/// 不承载任何业务数据。🔴 第十九轮评审 P0：key 必须带每次启动现生成的随机
/// nonce（见 `probe_key`）——固定 key 可能与业务对象冲突（启动即被覆盖/删除），
/// 多实例并发启动还会互相覆盖。
const PROBE_KEY_PREFIX: &str = "__privchat_probe__/capability";

/// 生成不可与业务 key 冲突的探测 key：专用前缀 + 用途 + 随机 UUID nonce。
fn probe_key(purpose: &str) -> String {
    format!("{PROBE_KEY_PREFIX}/{purpose}/{}", uuid::Uuid::new_v4().as_simple())
}

/// 生产 S3 控制面 + final 对象探测：一份连接配置同时实现两个冻结接口。
pub struct S3DirectBackend {
    client: reqwest::Client,
    signer: reqsign::AwsV4Signer,
    cred: reqsign::AwsCredential,
    /// 含 scheme 的 endpoint（如 `https://s3.e2e.local`）。
    endpoint: String,
    /// `ListParts` 页大小：生产固定 1000；真实集成门禁用小值验证分页循环。
    list_page_size: u32,
}

impl S3DirectBackend {
    /// 按存储源配置构建。🔴 只有 `storage_type = "s3"` 且 `direct_upload` 显式为
    /// `s3_multipart_v1` 的源才允许构建；字段缺失在启动期直接报错（fail-fast，
    /// 而不是第一次上传时才炸）。
    pub fn from_source(src: &FileStorageSourceConfig) -> Result<Self, ServerError> {
        if src.storage_type != "s3" {
            return Err(ServerError::Internal(format!(
                "存储源 id={} 不是 s3 类型，不能开启 direct_upload",
                src.id
            )));
        }
        let err = |what: &str| {
            ServerError::Internal(format!(
                "存储源 id={} 开启了 direct_upload 但缺少 {what}",
                src.id
            ))
        };
        let endpoint = src.endpoint.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| err("endpoint"))?;
        let bucket = src.bucket.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| err("bucket"))?;
        let access_key_id = src.access_key_id.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| err("access_key_id"))?;
        let secret_access_key = src.secret_access_key.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| err("secret_access_key"))?;
        let _ = bucket; // bucket 逐请求来自 UploadReference，这里只校验配置完整性。
        let endpoint = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.to_string()
        } else {
            format!("https://{endpoint}")
        };
        let region = src
            .region
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("us-east-1")
            .to_string();
        Ok(Self {
            client: reqwest::Client::new(),
            signer: reqsign::AwsV4Signer::new("s3", &region),
            cred: reqsign::AwsCredential {
                access_key_id: access_key_id.to_string(),
                secret_access_key: secret_access_key.to_string(),
                session_token: None,
                expires_in: None,
            },
            endpoint,
            list_page_size: 1000,
        })
    }

    /// 仅测试用：改小 `ListParts` 页大小，用少量分片即可验证分页循环。
    /// 生产代码从不调整，启动装配固定 1000。
    pub fn with_list_page_size(mut self, size: u32) -> Self {
        self.list_page_size = size.max(1);
        self
    }

    /// path-style 对象 URL：`{endpoint}/{bucket}/{key}`（key 逐段百分号编码）。
    fn object_url(&self, bucket: &str, key: &str) -> String {
        format!("{}/{}/{}", self.endpoint, bucket, encode_key(key))
    }

    /// 探测对象的清理：裸 DELETE（随机探测 key 归探测独占，无条件删是安全的，
    /// 也不依赖被探测的条件删除能力本身）。2xx/404 均视为已清。
    async fn probe_cleanup_object(&self, bucket: &str, key: &str) -> Result<(), ServerError> {
        let url = self.object_url(bucket, key);
        let req = self
            .client
            .delete(&url)
            .build()
            .map_err(|e| ServerError::Internal(format!("探测清理：构建 DELETE 请求失败: {e}")))?;
        let resp = self
            .signed_execute(req)
            .await
            .map_err(|e| ServerError::Internal(format!("探测清理：DELETE 失败: {e:?}")))?;
        let s = resp.status();
        if s.is_success() || s == StatusCode::NOT_FOUND {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ServerError::Internal(format!(
            "探测清理失败: HTTP {} body={}",
            s.as_u16(),
            truncate(&body, 256)
        )))
    }

    /// 探测 MPU 的清理：幂等 abort（`NoSuchUpload` = 已关，视为已清）。
    async fn probe_cleanup_upload(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<(), ServerError> {
        let reference = UploadReference {
            bucket: bucket.to_string(),
            final_key: key.to_string(),
            provider_upload_id: upload_id.to_string(),
        };
        <Self as NumberedPartBackend>::abort(&self, &reference)
            .await
            .map_err(|e| ServerError::Internal(format!("探测清理：abort 失败: {e:?}")))
    }

    /// 尽力清理（用于「反正已拒绝启动」的失败路径）：失败只告警，不掩盖原因。
    async fn probe_cleanup_best_effort(&self, bucket: &str, key: &str, upload_id: Option<&str>) {
        if let Some(id) = upload_id {
            if let Err(e) = self.probe_cleanup_upload(bucket, key, id).await {
                tracing::warn!("探测清理（尽力）abort 失败: {e}，探测 key={key}");
            }
        }
        if let Err(e) = self.probe_cleanup_object(bucket, key).await {
            tracing::warn!("探测清理（尽力）DELETE 失败: {e}，探测 key={key}");
        }
    }

    /// 🔴 启动期能力探测（第十八轮评审 P0）：验证后端是否真正支持
    /// `DeleteObject` 的 `If-Match` 条件。真实门禁发现 MinIO 会忽略条件直接删：
    /// 这种后端上「归属核对 + 条件删除」退化为无条件删，扫描器可能删到被替换的
    /// 对象——因此在启动期证明能力，不支持直接拒绝开启 `direct_upload`。
    ///
    /// 探测序列：PUT 探测对象 → 用**过期 ETag** 发条件删除：
    /// - 412 → 条件生效 = 支持（随后无条件清理随机探测对象）；
    /// - 2xx 且对象消失 → 条件被忽略，旧条件删掉了新对象 = **不安全**；
    /// - 其余（2xx 但对象仍在、传输错误等）→ 行为不可预测，同样拒绝。
    pub async fn probe_conditional_delete(&self, bucket: &str) -> Result<bool, ServerError> {
        const CTX: &str = "条件删除能力探测";
        // 🔴 第十九轮评审 P0：每次探测现生成随机 key——不撞业务对象、实例间不互踩、
        // 不留固定残留。
        let key = probe_key("conditional-delete");
        let url = self.object_url(bucket, &key);
        let req = self
            .client
            .put(&url)
            .header("content-length", "1")
            .body(vec![0u8])
            .build()
            .map_err(|e| ServerError::Internal(format!("{CTX}：构建 PUT 请求失败: {e}")))?;
        let resp = self.signed_execute(req).await.map_err(|e| {
            ServerError::Internal(format!("{CTX}：PUT 探测对象失败: {e:?}"))
        })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ServerError::Internal(format!(
                "{CTX}：PUT 探测对象失败: HTTP {} body={}",
                status.as_u16(),
                truncate(&body, 256)
            )));
        }
        // 2. HEAD 确认对象落盘。
        let reference = UploadReference {
            bucket: bucket.to_string(),
            final_key: key.clone(),
            provider_upload_id: String::new(),
        };
        let head = <Self as FinalObjectProbe>::head(&self, &reference)
            .await
            .map_err(|e| ServerError::Internal(format!("{CTX}：HEAD 探测对象失败: {e}")))?
            .ok_or_else(|| {
                ServerError::Internal(format!("{CTX}：PUT 成功但 HEAD 不到探测对象"))
            })?;
        let _ = head; // 只需确认对象落盘；清理由探测自管（随机 key 无条件删）。
        // 3. 用过期 ETag 发条件删除，判定条件是否生效。
        let req = self
            .client
            .delete(&url)
            .header("if-match", "\"privchat-probe-stale-etag\"")
            .build()
            .map_err(|e| ServerError::Internal(format!("{CTX}：构建 DELETE 请求失败: {e}")))?;
        let resp = self.signed_execute(req).await.map_err(|e| {
            ServerError::Internal(format!("{CTX}：DELETE 探测请求失败: {e:?}"))
        })?;
        match resp.status() {
            StatusCode::PRECONDITION_FAILED => {
                // 条件生效 = 支持。🔴 第十九轮评审 P1：清理也是门禁的一部分——
                // 清理失败必须拒绝启动，不得静默放行（留下探测对象）。
                self.probe_cleanup_object(bucket, &key).await.map_err(|e| {
                    ServerError::Internal(format!(
                        "{CTX}：探测成功但清理失败: {e}，探测 key={key}，拒绝启动"
                    ))
                })?;
                Ok(true)
            }
            s if s.is_success() => {
                let gone = <Self as FinalObjectProbe>::head(&self, &reference)
                    .await
                    .map_err(|e| {
                        ServerError::Internal(format!("{CTX}：删后核验 HEAD 失败: {e}"))
                    })?
                    .is_none();
                if gone {
                    // 旧条件直接删掉了新对象：条件删除安全保证失效（对象已被无条件删掉，无残留）。
                    Ok(false)
                } else {
                    // 删除返回成功但对象仍在：行为不可预测，拒绝。反正拒绝启动，清理尽力即可。
                    self.probe_cleanup_best_effort(bucket, &key, None).await;
                    Err(ServerError::Internal(format!(
                        "{CTX}：过期 ETag 删除返回成功但对象仍在，后端行为不可预测，拒绝开启 direct_upload"
                    )))
                }
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                self.probe_cleanup_best_effort(bucket, &key, None).await;
                Err(ServerError::Internal(format!(
                    "{CTX}：意外的响应: HTTP {} body={}",
                    s.as_u16(),
                    truncate(&body, 256)
                )))
            }
        }
    }

    /// 🔴 启动期能力探测（第十九轮评审 P0）：验证后端是否真正支持
    /// `CompleteMultipartUpload` 的 `If-None-Match: *`（final key no-clobber，§8.5）。
    /// 直传安全同时依赖它：后端忽略该条件时，并发 complete 会覆盖已有正式对象。
    ///
    /// 探测序列：PUT 一个「已有正式对象」→ 同 key 建 MPU → 传一片 → 带
    /// `If-None-Match: *` 的 Complete：
    /// - 409/412 → 条件生效 = 支持（清理 MPU + 探测对象后放行）；
    /// - 2xx（对象被覆盖）→ 条件被忽略 = **不安全**；
    /// - 其余行为不可预测同样拒绝。🔴 清理失败同样拒绝启动（第十九轮评审 P1）。
    pub async fn probe_complete_no_clobber(&self, bucket: &str) -> Result<bool, ServerError> {
        const CTX: &str = "complete no-clobber 能力探测";
        let key = probe_key("complete-no-clobber");
        let url = self.object_url(bucket, &key);
        // 1. PUT 一个「已有正式对象」（探测要保护的对象）。
        let req = self
            .client
            .put(&url)
            .header("content-length", "1")
            .body(vec![0u8])
            .build()
            .map_err(|e| ServerError::Internal(format!("{CTX}：构建 PUT 请求失败: {e}")))?;
        let resp = self
            .signed_execute(req)
            .await
            .map_err(|e| ServerError::Internal(format!("{CTX}：PUT 探测对象失败: {e:?}")))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ServerError::Internal(format!(
                "{CTX}：PUT 探测对象失败: HTTP {} body={}",
                status.as_u16(),
                truncate(&body, 256)
            )));
        }
        // 2. 同一 key 建 MPU（不声明 checksum 算法：探测不牵涉业务 checksum 链路）。
        let req = self
            .client
            .post(format!("{url}?uploads="))
            .header("content-type", "application/octet-stream")
            .build()
            .map_err(|e| ServerError::Internal(format!("{CTX}：构建 CreateMPU 请求失败: {e}")))?;
        let resp = self
            .signed_execute(req)
            .await
            .map_err(|e| ServerError::Internal(format!("{CTX}：CreateMPU 失败: {e:?}")))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            self.probe_cleanup_best_effort(bucket, &key, None).await;
            return Err(ServerError::Internal(format!(
                "{CTX}：CreateMPU 失败: HTTP {} body={}",
                status.as_u16(),
                truncate(&body, 256)
            )));
        }
        let upload_id = match xml_text(&body, "UploadId").filter(|s| !s.is_empty()) {
            Some(id) => id,
            None => {
                self.probe_cleanup_best_effort(bucket, &key, None).await;
                return Err(ServerError::Internal(format!(
                    "{CTX}：CreateMPU 响应缺少 UploadId"
                )));
            }
        };
        // 3. 传一片（部分后端对空分片列表的 complete 先报参数错，走不到条件判定）。
        let req = self
            .client
            .put(format!("{url}?partNumber=1&uploadId={upload_id}"))
            .header("content-length", "1")
            .body(vec![0u8])
            .build()
            .map_err(|e| ServerError::Internal(format!("{CTX}：构建 UploadPart 请求失败: {e}")))?;
        let resp = match self.signed_execute(req).await {
            Ok(r) => r,
            Err(e) => {
                self.probe_cleanup_best_effort(bucket, &key, Some(&upload_id)).await;
                return Err(ServerError::Internal(format!("{CTX}：UploadPart 失败: {e:?}")));
            }
        };
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            self.probe_cleanup_best_effort(bucket, &key, Some(&upload_id)).await;
            return Err(ServerError::Internal(format!(
                "{CTX}：UploadPart 失败: HTTP {} body={}",
                status.as_u16(),
                truncate(&body, 256)
            )));
        }
        let part_etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if part_etag.is_empty() {
            self.probe_cleanup_best_effort(bucket, &key, Some(&upload_id)).await;
            return Err(ServerError::Internal(format!(
                "{CTX}：UploadPart 响应缺少 ETag"
            )));
        }
        // 4. 带 If-None-Match: * 的 Complete：已有对象在，必须被拒。
        let complete_xml = format!(
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part_etag}</ETag></Part></CompleteMultipartUpload>"
        );
        let req = self
            .client
            .post(format!("{url}?uploadId={upload_id}"))
            .header("if-none-match", "*")
            .header("content-type", "application/xml")
            .body(complete_xml)
            .build()
            .map_err(|e| ServerError::Internal(format!("{CTX}：构建 CompleteMPU 请求失败: {e}")))?;
        let resp = match self.signed_execute(req).await {
            Ok(r) => r,
            Err(e) => {
                self.probe_cleanup_best_effort(bucket, &key, Some(&upload_id)).await;
                return Err(ServerError::Internal(format!("{CTX}：CompleteMPU 请求失败: {e:?}")));
            }
        };
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        match status {
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => {
                // 条件生效 = 支持。🔴 清理是门禁的一部分（第十九轮评审 P1）：
                // abort MPU + 删探测对象，失败拒绝启动。
                self.probe_cleanup_upload(bucket, &key, &upload_id).await.map_err(|e| {
                    ServerError::Internal(format!(
                        "{CTX}：探测成功但 MPU 清理失败: {e}，探测 key={key}，拒绝启动"
                    ))
                })?;
                self.probe_cleanup_object(bucket, &key).await.map_err(|e| {
                    ServerError::Internal(format!(
                        "{CTX}：探测成功但清理失败: {e}，探测 key={key}，拒绝启动"
                    ))
                })?;
                Ok(true)
            }
            s if s.is_success() => {
                if let Some(code) = xml_text(&body, "Code") {
                    // 200 里包错误：不是探测预期的行为，拒绝。
                    self.probe_cleanup_best_effort(bucket, &key, Some(&upload_id)).await;
                    return Err(ServerError::Internal(format!(
                        "{CTX}：CompleteMPU 返回 200 但含错误 code={code} body={}",
                        truncate(&body, 256)
                    )));
                }
                // 对象被覆盖：If-None-Match 被忽略 = 不安全。MPU 已被 complete 消费。
                // 清理仍是门禁的一部分：删掉被覆盖的探测对象，失败拒绝启动。
                self.probe_cleanup_object(bucket, &key).await.map_err(|e| {
                    ServerError::Internal(format!(
                        "{CTX}：清理失败: {e}，探测 key={key}，拒绝启动"
                    ))
                })?;
                Ok(false)
            }
            s => {
                self.probe_cleanup_best_effort(bucket, &key, Some(&upload_id)).await;
                Err(ServerError::Internal(format!(
                    "{CTX}：意外的响应: HTTP {} body={}",
                    s.as_u16(),
                    truncate(&body, 256)
                )))
            }
        }
    }

    /// 签名并执行：签名必须在设置完全部参与签名的头之后。
    async fn signed_execute(
        &self,
        mut req: reqwest::Request,
    ) -> Result<reqwest::Response, NumberedPartError> {
        self.signer
            .sign(&mut req, &self.cred)
            .map_err(|e| NumberedPartError::Backend(format!("SigV4 签名失败: {e}")))?;
        self.client
            .execute(req)
            .await
            .map_err(|e| NumberedPartError::Backend(format!("S3 请求失败: {e}")))
    }
}

/// object key 百分号编码：仅放行未保留字符与 `/`（S3 canonical 要求）。
fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for &b in key.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// S3 错误体里的 `<Code>`；解析失败返回空串。
fn xml_error_code(body: &str) -> String {
    xml_text(body, "Code").unwrap_or_default()
}

fn map_error(status: StatusCode, body: &str, op: &str) -> NumberedPartError {
    if xml_error_code(body) == "NoSuchUpload" {
        return NumberedPartError::NoSuchUpload;
    }
    NumberedPartError::Backend(format!(
        "{op}: HTTP {} body={}",
        status.as_u16(),
        truncate(body, 512)
    ))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // 按字符边界截断，避免切到半个 UTF-8 字符。
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// 从 XML 文档取第一个 `<tag>` 的文本内容（S3 响应小而标签名唯一，扁平扫描够用）。
fn xml_text(xml: &str, tag: &str) -> Option<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(xml.as_bytes());
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.local_name().as_ref() == tag.as_bytes() => {
                let end = e.name().to_owned();
                return reader.read_text(end).ok().map(|t| t.into_owned());
            }
            Ok(Event::Empty(e)) if e.local_name().as_ref() == tag.as_bytes() => {
                return Some(String::new());
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

/// 取 `<tag>` 文本的通用小助手（解析器游标已在对应 Start 事件上）。
fn read_tag_text(reader: &mut quick_xml::Reader<&[u8]>, e: &quick_xml::events::BytesStart) -> Option<String> {
    let end = e.name().to_owned();
    reader.read_text(end).ok().map(|t| t.into_owned())
}

/// 解析 `ListParts` 响应：`(分片列表, 是否截断, 下一页 marker)`。
fn parse_list_parts(xml: &str) -> (Vec<ListedPart>, bool, Option<String>) {
    use quick_xml::events::Event;
    let mut parts: Vec<ListedPart> = Vec::new();
    let mut truncated = false;
    let mut marker: Option<String> = None;
    let mut current: Option<ListedPart> = None;

    let mut reader = quick_xml::Reader::from_reader(xml.as_bytes());
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let local = e.local_name();
                let name = local.as_ref();
                if name == b"Part" {
                    current = Some(ListedPart {
                        part_number: 0,
                        size: 0,
                        etag: String::new(),
                        checksum_sha256_b64: None,
                    });
                } else if let Some(cur) = current.as_mut() {
                    match name {
                        b"PartNumber" => {
                            if let Some(t) = read_tag_text(&mut reader, &e) {
                                cur.part_number = t.trim().parse().unwrap_or(0);
                            }
                        }
                        b"Size" => {
                            if let Some(t) = read_tag_text(&mut reader, &e) {
                                cur.size = t.trim().parse().unwrap_or(0);
                            }
                        }
                        b"ETag" => {
                            if let Some(t) = read_tag_text(&mut reader, &e) {
                                cur.etag = t.trim().to_string();
                            }
                        }
                        b"ChecksumSHA256" => {
                            if let Some(t) = read_tag_text(&mut reader, &e) {
                                cur.checksum_sha256_b64 = Some(t.trim().to_string());
                            }
                        }
                        _ => {}
                    }
                } else if name == b"IsTruncated" {
                    if let Some(t) = read_tag_text(&mut reader, &e) {
                        truncated = t.trim() == "true";
                    }
                } else if name == b"NextPartNumberMarker" {
                    if let Some(t) = read_tag_text(&mut reader, &e) {
                        marker = Some(t.trim().to_string());
                    }
                }
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"Part" => {
                if let Some(p) = current.take() {
                    parts.push(p);
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    (parts, truncated, marker)
}

/// 拼 `CompleteMultipartUpload` 请求体：🔴 每片必须三字段齐全（类型已保证）。
fn complete_body(parts: &[CompletedPart]) -> String {
    let mut xml = String::from("<CompleteMultipartUpload>");
    for p in parts {
        xml.push_str("<Part>");
        xml.push_str(&format!("<PartNumber>{}</PartNumber>", p.part_number));
        xml.push_str(&format!("<ETag>{}</ETag>", p.etag));
        xml.push_str(&format!(
            "<ChecksumSHA256>{}</ChecksumSHA256>",
            p.checksum_sha256_b64
        ));
        xml.push_str("</Part>");
    }
    xml.push_str("</CompleteMultipartUpload>");
    xml
}

#[async_trait]
impl NumberedPartBackend for S3DirectBackend {
    async fn create(
        &self,
        session_upload_id: &str,
        bucket: &str,
        final_key: &str,
        _total_size: u64,
    ) -> Result<UploadReference, NumberedPartError> {
        let url = format!("{}?uploads=", self.object_url(bucket, final_key));
        let req = self
            .client
            .post(&url)
            // 🔴 归属证明的源头（§2.2）：最终对象 metadata 写入会话 id。
            .header("x-amz-meta-privchat-upload-id", session_upload_id)
            // 🔴 声明逐片 SHA256：之后每片 checksum 才可查可验（§2.2）。
            .header("x-amz-checksum-algorithm", "SHA256")
            .header("content-type", "application/octet-stream")
            .build()
            .map_err(|e| NumberedPartError::Backend(format!("构建 CreateMPU 请求失败: {e}")))?;
        let resp = self.signed_execute(req).await?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| NumberedPartError::Backend(format!("读 CreateMPU 响应失败: {e}")))?;
        if !status.is_success() {
            return Err(map_error(status, &body, "CreateMultipartUpload"));
        }
        let provider_upload_id = xml_text(&body, "UploadId")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                NumberedPartError::Backend("CreateMultipartUpload 响应缺少 UploadId".to_string())
            })?;
        Ok(UploadReference {
            bucket: bucket.to_string(),
            final_key: final_key.to_string(),
            provider_upload_id,
        })
    }

    async fn sign_part_url(
        &self,
        reference: &UploadReference,
        part_number: u32,
        _content_length: u64,
        checksum_sha256_b64: &str,
        ttl_secs: u64,
    ) -> Result<String, NumberedPartError> {
        let url = format!(
            "{}?partNumber={}&uploadId={}",
            self.object_url(&reference.bucket, &reference.final_key),
            part_number,
            reference.provider_upload_id
        );
        // 🔴 checksum 签进 URL（§8.3）：该头成为签名头，客户端 PUT 时必须原样携带
        // （响应里的 required_headers 就是为这个服务的）。
        let mut req = self
            .client
            .put(&url)
            .header("x-amz-checksum-sha256", checksum_sha256_b64)
            .build()
            .map_err(|e| NumberedPartError::Backend(format!("构建 UploadPart 预签名请求失败: {e}")))?;
        self.signer
            .sign_query(&mut req, Duration::from_secs(ttl_secs), &self.cred)
            .map_err(|e| NumberedPartError::Backend(format!("预签名失败: {e}")))?;
        Ok(req.url().to_string())
    }

    async fn list_parts(
        &self,
        reference: &UploadReference,
    ) -> Result<Vec<ListedPart>, NumberedPartError> {
        let mut all = Vec::new();
        let mut marker: Option<String> = None;
        // 1000 片一页；冻结几何保证 ≤ 10000 片，最多 10 页。
        for _ in 0..10 {
            let mut url = format!(
                "{}?uploadId={}&max-parts={}",
                self.object_url(&reference.bucket, &reference.final_key),
                reference.provider_upload_id,
                self.list_page_size
            );
            if let Some(m) = &marker {
                url.push_str(&format!("&part-number-marker={m}"));
            }
            let req = self
                .client
                .get(&url)
                .build()
                .map_err(|e| NumberedPartError::Backend(format!("构建 ListParts 请求失败: {e}")))?;
            let resp = self.signed_execute(req).await?;
            let status = resp.status();
            let body = resp
                .text()
                .await
                .map_err(|e| NumberedPartError::Backend(format!("读 ListParts 响应失败: {e}")))?;
            if !status.is_success() {
                return Err(map_error(status, &body, "ListParts"));
            }
            let (parts, truncated, next) = parse_list_parts(&body);
            all.extend(parts);
            if !truncated {
                return Ok(all);
            }
            marker = next;
        }
        Err(NumberedPartError::Backend(
            "ListParts 分页超过 10 页仍未结束（分片数异常）".to_string(),
        ))
    }

    async fn complete(
        &self,
        reference: &UploadReference,
        parts: &[CompletedPart],
    ) -> Result<(), NumberedPartError> {
        let url = format!(
            "{}?uploadId={}",
            self.object_url(&reference.bucket, &reference.final_key),
            reference.provider_upload_id
        );
        let req = self
            .client
            .post(&url)
            // 🔴 final key 的 no-clobber 是存储层职责（§8.5）：
            // 409 → MPU 作废（20618 重来）；412 → 已有对象（核验复用），不得混同。
            .header("if-none-match", "*")
            .header("content-type", "application/xml")
            .body(complete_body(parts))
            .build()
            .map_err(|e| NumberedPartError::Backend(format!("构建 CompleteMPU 请求失败: {e}")))?;
        let resp = self.signed_execute(req).await?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| NumberedPartError::Backend(format!("读 CompleteMPU 响应失败: {e}")))?;
        match status {
            StatusCode::CONFLICT => return Err(NumberedPartError::Conflict),
            StatusCode::PRECONDITION_FAILED => return Err(NumberedPartError::PreconditionFailed),
            s if s.is_success() => {
                // S3 经典行为：200 里也可能包 <Error>，必须看体。
                if let Some(code) = xml_text(&body, "Code") {
                    if code == "NoSuchUpload" {
                        return Err(NumberedPartError::NoSuchUpload);
                    }
                    return Err(NumberedPartError::Backend(format!(
                        "CompleteMultipartUpload: HTTP 200 但含错误 code={code} body={}",
                        truncate(&body, 512)
                    )));
                }
                Ok(())
            }
            _ => Err(map_error(status, &body, "CompleteMultipartUpload")),
        }
    }

    async fn abort(&self, reference: &UploadReference) -> Result<(), NumberedPartError> {
        let url = format!(
            "{}?uploadId={}",
            self.object_url(&reference.bucket, &reference.final_key),
            reference.provider_upload_id
        );
        let req = self
            .client
            .delete(&url)
            .build()
            .map_err(|e| NumberedPartError::Backend(format!("构建 AbortMPU 请求失败: {e}")))?;
        let resp = self.signed_execute(req).await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        // 🔴 幂等：NoSuchUpload = 已关闭，视为成功。
        if status == StatusCode::NOT_FOUND && xml_error_code(&body) == "NoSuchUpload" {
            return Ok(());
        }
        Err(map_error(status, &body, "AbortMultipartUpload"))
    }
}

#[async_trait]
impl FinalObjectProbe for S3DirectBackend {
    async fn head(
        &self,
        reference: &UploadReference,
    ) -> Result<Option<FinalObjectHead>, ProbeError> {
        let url = self.object_url(&reference.bucket, &reference.final_key);
        let mut req = self
            .client
            .head(&url)
            .build()
            .map_err(|e| ProbeError::Backend(format!("构建 HEAD 请求失败: {e}")))?;
        self.signer
            .sign(&mut req, &self.cred)
            .map_err(|e| ProbeError::Backend(format!("SigV4 签名失败: {e}")))?;
        let resp = self
            .client
            .execute(req)
            .await
            .map_err(|e| ProbeError::Backend(format!("HEAD 请求失败: {e}")))?;
        match resp.status() {
            StatusCode::NOT_FOUND => Ok(None),
            s if s.is_success() => {
                let content_length = resp
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                // 🔴 归属证明：对象 metadata `privchat-upload-id`（CreateMPU 时写入）。
                let privchat_upload_id = resp
                    .headers()
                    .get("x-amz-meta-privchat-upload-id")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.to_string());
                // 🔴 条件删除的凭据：后续 delete_if_match 以它为条件（防 TOCTOU）。
                let etag = resp
                    .headers()
                    .get("etag")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                if etag.is_empty() {
                    return Err(ProbeError::Backend("HEAD 响应缺少 ETag".to_string()));
                }
                Ok(Some(FinalObjectHead {
                    content_length,
                    privchat_upload_id,
                    etag,
                }))
            }
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(ProbeError::Backend(format!(
                    "HEAD final 对象失败: HTTP {} body={}",
                    s.as_u16(),
                    truncate(&body, 512)
                )))
            }
        }
    }

    async fn sha256_of(&self, reference: &UploadReference) -> Result<String, ProbeError> {
        let url = self.object_url(&reference.bucket, &reference.final_key);
        let mut req = self
            .client
            .get(&url)
            .build()
            .map_err(|e| ProbeError::Backend(format!("构建 GET 请求失败: {e}")))?;
        self.signer
            .sign(&mut req, &self.cred)
            .map_err(|e| ProbeError::Backend(format!("SigV4 签名失败: {e}")))?;
        let mut resp = self
            .client
            .execute(req)
            .await
            .map_err(|e| ProbeError::Backend(format!("GET 回读失败: {e}")))?;
        if !resp.status().is_success() {
            return Err(ProbeError::Backend(format!(
                "GET 回读 final 对象失败: HTTP {}",
                resp.status().as_u16()
            )));
        }
        // 🔴 流式：全程只驻留单个网络 chunk 的内存；S3 multipart 的 ETag/复合
        // SHA-256 不等于整文件摘要，文件身份唯一权威就是这次回读（§3.5）。
        let mut hasher = sha2::Sha256::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ProbeError::Backend(format!("回读分块失败: {e}")))?
        {
            use sha2::Digest as _;
            hasher.update(&chunk);
        }
        use sha2::Digest as _;
        Ok(hex::encode(hasher.finalize()))
    }

    async fn delete_if_match(
        &self,
        reference: &UploadReference,
        etag: &str,
    ) -> Result<bool, ProbeError> {
        let url = self.object_url(&reference.bucket, &reference.final_key);
        let mut req = self
            .client
            .delete(&url)
            // 🔴 条件删除（§8.5 统一删除规则）：ETag 不符 → 412，拒绝。
            .header("if-match", etag)
            .build()
            .map_err(|e| ProbeError::Backend(format!("构建 DELETE 请求失败: {e}")))?;
        self.signer
            .sign(&mut req, &self.cred)
            .map_err(|e| ProbeError::Backend(format!("SigV4 签名失败: {e}")))?;
        let resp = self
            .client
            .execute(req)
            .await
            .map_err(|e| ProbeError::Backend(format!("DELETE 请求失败: {e}")))?;
        match resp.status() {
            // 🔴 第十七轮评审 P1（真实 MinIO 门禁发现）：不是所有兼容服务都支持
            // DELETE 的 If-Match（MinIO 会忽略条件直接删）。因此 2xx 后必须再 HEAD：
            // 对象消失 = 删除成立；对象仍在 = 条件被忽略且对象已被替换，返回拒绝。
            s if s.is_success() => match self.head(reference).await {
                Ok(None) => Ok(true),
                Ok(Some(_)) => Ok(false),
                Err(e) => Err(ProbeError::Backend(format!(
                    "删除后核验 HEAD 失败，不能确认删除结果: {e}"
                ))),
            },
            // 对象已不在：本轮目标就是「对象消失」，幂等达成。
            StatusCode::NOT_FOUND => Ok(true),
            StatusCode::PRECONDITION_FAILED => Ok(false), // ETag 不符：拒绝删除。
            s => {
                let body = resp.text().await.unwrap_or_default();
                Err(ProbeError::Backend(format!(
                    "条件删除 final 对象失败: HTTP {} body={}",
                    s.as_u16(),
                    truncate(&body, 512)
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_key_keeps_unreserved_and_slashes() {
        assert_eq!(encode_key("files/123.bin"), "files/123.bin");
        assert_eq!(encode_key("a b/c+d"), "a%20b/c%2Bd");
        assert_eq!(encode_key("中文/文件"), "%E4%B8%AD%E6%96%87/%E6%96%87%E4%BB%B6");
    }

    /// 🔴 第十九轮评审 P0：探测 key 必须随机且互斥——不能用固定生产 key，
    /// 否则启动探测会覆盖/删除真实业务对象，多实例启动还会互踩。
    #[test]
    fn probe_keys_are_random_and_isolated() {
        let a = probe_key("conditional-delete");
        let b = probe_key("conditional-delete");
        assert_ne!(a, b, "每次探测必须现生成随机 key");
        for k in [&a, &b] {
            assert!(
                k.starts_with("__privchat_probe__/capability/"),
                "探测 key 必须落在专用前缀下: {k}"
            );
        }
        assert_ne!(
            probe_key("complete-no-clobber"),
            probe_key("conditional-delete"),
            "不同用途的探测 key 互斥"
        );
    }

    #[test]
    fn xml_text_picks_the_first_matching_tag() {
        let body = r#"<?xml version="1.0"?>
<InitiateMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Bucket>b</Bucket><Key>k</Key><UploadId>upid-42</UploadId>
</InitiateMultipartUploadResult>"#;
        assert_eq!(xml_text(body, "UploadId").as_deref(), Some("upid-42"));
        assert_eq!(xml_text(body, "Bucket").as_deref(), Some("b"));
        assert_eq!(xml_text(body, "Nope"), None);
    }

    #[test]
    fn parse_list_parts_handles_pagination_and_checksums() {
        let body = r#"<ListPartsResult>
  <IsTruncated>true</IsTruncated>
  <NextPartNumberMarker>2</NextPartNumberMarker>
  <Part><PartNumber>1</PartNumber><Size>8388608</Size><ETag>"e1"</ETag><ChecksumSHA256>c1</ChecksumSHA256></Part>
  <Part><PartNumber>2</PartNumber><Size>123</Size><ETag>"e2"</ETag></Part>
</ListPartsResult>"#;
        let (parts, truncated, marker) = parse_list_parts(body);
        assert!(truncated);
        assert_eq!(marker.as_deref(), Some("2"));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].part_number, 1);
        assert_eq!(parts[0].size, 8388608);
        assert_eq!(parts[0].etag, "\"e1\"");
        assert_eq!(parts[0].checksum_sha256_b64.as_deref(), Some("c1"));
        assert_eq!(parts[1].checksum_sha256_b64, None, "缺 checksum = None，调用方按缺失处理");
    }

    #[test]
    fn complete_body_carries_all_three_fields_per_part() {
        let xml = complete_body(&[CompletedPart {
            part_number: 3,
            etag: "\"e3\"".to_string(),
            checksum_sha256_b64: "c3".to_string(),
        }]);
        assert!(xml.contains("<PartNumber>3</PartNumber>"));
        assert!(xml.contains("<ETag>\"e3\"</ETag>"));
        assert!(xml.contains("<ChecksumSHA256>c3</ChecksumSHA256>"));
        assert!(xml.starts_with("<CompleteMultipartUpload>"));
        assert!(xml.ends_with("</CompleteMultipartUpload>"));
    }

    #[test]
    fn error_mapping_distinguishes_no_such_upload() {
        let body = r#"<Error><Code>NoSuchUpload</Code><Message>x</Message></Error>"#;
        assert!(matches!(
            map_error(StatusCode::NOT_FOUND, body, "Abort"),
            NumberedPartError::NoSuchUpload
        ));
        assert!(matches!(
            map_error(StatusCode::INTERNAL_SERVER_ERROR, "<Error><Code>SlowDown</Code></Error>", "X"),
            NumberedPartError::Backend(_)
        ));
    }

    /// 配置门禁：非 s3 / 缺字段都必须在启动期报错（第十六轮评审：接线必须
    /// fail-fast，不能拖到第一次上传）。
    #[test]
    fn from_source_rejects_bad_config() {
        let base = FileStorageSourceConfig {
            id: 1,
            storage_type: "local".to_string(),
            storage_root: "/tmp".to_string(),
            base_url: None,
            endpoint: Some("s3.local".to_string()),
            bucket: Some("b".to_string()),
            access_key_id: Some("ak".to_string()),
            secret_access_key: Some("sk".to_string()),
            path_prefix: None,
            direct_upload: Some(DIRECT_UPLOAD_S3_MULTIPART_V1.to_string()),
            region: None,
        };
        assert!(S3DirectBackend::from_source(&base).is_err(), "local 源不允许");
        let mut ok = base.clone();
        ok.storage_type = "s3".to_string();
        assert!(S3DirectBackend::from_source(&ok).is_ok());
        let mut no_bucket = ok.clone();
        no_bucket.bucket = None;
        assert!(S3DirectBackend::from_source(&no_bucket).is_err());
        let mut no_key = ok.clone();
        no_key.secret_access_key = Some("  ".to_string());
        assert!(S3DirectBackend::from_source(&no_key).is_err());
    }
}
