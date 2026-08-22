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
