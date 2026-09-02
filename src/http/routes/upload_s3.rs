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

//! S3 直传的 status/complete/abort 分流（RESUMABLE_UPLOAD_SPEC §8.3/§8.5，
//! 实现顺序第 3 步）。
//!
//! 端点路径不新增：服务端读 `manifest.transport` 分流，proxy 路径一行不动。
//! 🔴 complete 的 409/412 恢复动作必须取自 [`complete_recovery_for`]（冻结语义
//! 唯一真源），不得按 HTTP 码猜。

use std::sync::Arc;

use privchat_protocol::ErrorCode as E;

use crate::error::ServerError;
use crate::http::{ApiEnvelope, ApiResult, FileServerState};
use crate::service::chunked_upload::{ChunkedSession, Manifest, Range};
use crate::service::final_object_probe::{FinalObjectHead, FinalObjectProbe, ProbeError};
use crate::service::numbered_parts::{
    check_part_geometry, complete_recovery_for, CompletedPart, CompleteRecovery,
    NumberedPartBackend, NumberedPartError, UploadReference,
};

use super::upload::{
    chunked_completed_response, client_ip_from_headers, coded, lock_or_busy, manifest_matches,
    upload_response_of, ChunkedStatusResponse, CompleteRequest, UploadResponse,
};

/// 20618：会话作废，客户端废弃它从零重新申请（HTTP 422，终局）。
fn restart_required(msg: impl Into<String>) -> ServerError {
    coded(E::UploadRestartRequired, 422, msg)
}

/// manifest 平铺字段 → MPU 引用。🔴 五个字段作为整体原子使用（§8.7），
/// 缺一即会话半建，按内部错误处理。part-url 与三个分流共用这一处提取。
pub(super) fn s3_reference_of(
    manifest: &Manifest,
) -> Result<(u64, u32, UploadReference), ServerError> {
    match (
        manifest.part_size,
        manifest.total_parts,
        manifest.bucket.as_deref(),
        manifest.final_key.as_deref(),
        manifest.provider_upload_id.as_deref(),
    ) {
        (Some(part_size), Some(total_parts), Some(bucket), Some(final_key), Some(pid)) => Ok((
            part_size,
            total_parts,
            UploadReference {
                bucket: bucket.to_string(),
                final_key: final_key.to_string(),
                provider_upload_id: pid.to_string(),
            },
        )),
        _ => Err(ServerError::Internal(
            "S3 会话缺少 part_size/total_parts/bucket/final_key/provider_upload_id".to_string(),
        )),
    }
}

fn numbered_backend(
    state: &FileServerState,
) -> Result<&Arc<dyn NumberedPartBackend>, ServerError> {
    state.numbered_part_backend.as_ref().ok_or_else(|| {
        ServerError::Internal("直传后端未配置（direct_upload 门禁未接入）".to_string())
    })
}

fn object_probe(state: &FileServerState) -> Result<&Arc<dyn FinalObjectProbe>, ServerError> {
    state.final_object_probe.as_ref().ok_or_else(|| {
        ServerError::Internal("对象探测未配置（direct_upload 门禁未接入）".to_string())
    })
}

fn probe_err(e: ProbeError, ctx: &str) -> ServerError {
    ServerError::Internal(format!("{ctx}: {e}"))
}

/// list_parts 的非 NoSuchUpload 错误在 status/complete 里都不是冻结分支：
/// Backend 可重试，Conflict/PreconditionFailed 不该出现在列片阶段。
fn list_err(e: &NumberedPartError, ctx: &str) -> ServerError {
    match e {
        NumberedPartError::Backend(m) => ServerError::Internal(format!("{ctx}: {m}")),
        other => ServerError::Internal(format!("{ctx}遇到意外错误: {other:?}")),
    }
}

/// `ListParts` → received 区间（§8.3）：🔴 换算前逐片校验——几何异常
/// （part_number 越界 / 长度不符）的片一律视为缺失（不进 received，留给
/// 客户端重传），不得产出语义不明的区间。🔴 第二十九轮：换算依据只有几何证据，
/// 不依赖逐片 checksum 回读（COS 的 ListParts 不回逐片摘要）。
fn ranges_from_parts(
    parts: &[crate::service::numbered_parts::ListedPart],
    part_size: u64,
    total_parts: u32,
    total_size: u64,
) -> Vec<Range> {
    let mut ranges: Vec<Range> = parts
        .iter()
        .filter(|p| {
            check_part_geometry(p.part_number, p.size, total_parts, part_size, total_size)
                .is_ok()
        })
        .map(|p| Range {
            offset: (p.part_number as u64 - 1) * part_size,
            length: p.size,
        })
        .collect();
    ranges.sort_by_key(|r| r.offset);
    // 相邻合并（与 ChunkedSession::status 同口径）；重复片号按最后写入者覆盖，
    // 这里只保留并集端点，防御性处理。
    let mut merged: Vec<Range> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if last.offset + last.length >= r.offset => {
                let end = (last.offset + last.length).max(r.offset + r.length);
                last.length = end - last.offset;
            }
            _ => merged.push(r),
        }
    }
    merged
}

/// received 的补集 = missing（与 `ChunkedSession::status` 同口径）。
fn missing_complement(received: &[Range], total_size: u64) -> Vec<Range> {
    let mut missing = Vec::new();
    let mut cursor = 0u64;
    for r in received {
        if r.offset > cursor {
            missing.push(Range {
                offset: cursor,
                length: r.offset - cursor,
            });
        }
        cursor = cursor.max(r.offset + r.length);
    }
    if cursor < total_size {
        missing.push(Range {
            offset: cursor,
            length: total_size - cursor,
        });
    }
    missing
}

/// 建行/复用前的强制幂等 abort：`NoSuchUpload` 视为成功；🔴 其他失败不得
/// 吞掉（第十五轮评审 P1）——abort 失败仍建行写墓碑，等于把「清理失败」
/// 记成「上传完成」，旧 MPU 与预签名 URL 会一直存活。失败回可重试 5xx，
/// 会话保留（此刻尚未建行/写墓碑，重试无损）。
async fn must_abort(
    backend: &Arc<dyn NumberedPartBackend>,
    reference: &UploadReference,
) -> Result<(), ServerError> {
    match backend.abort(reference).await {
        Ok(()) | Err(NumberedPartError::NoSuchUpload) => Ok(()),
        Err(e) => Err(ServerError::Internal(format!(
            "幂等 abort MPU 失败，会话保留，请稍后重试: {e:?}"
        ))),
    }
}

/// `GET /files/status` 的 S3 分支（§8.3）。🔴 不改 status 协议：客户端断点
/// 恢复逻辑零改动。
pub(super) async fn s3_status(
    state: &FileServerState,
    session: &ChunkedSession,
) -> ApiResult<ChunkedStatusResponse> {
    // flock 保护服务端动作（§8.4）：与 part-url 签发/complete/abort 互斥。
    let _lock = lock_or_busy(session).await?;
    let total = session.manifest().total_size;

    if session.completed_file_id()?.is_some() {
        return Ok(ApiEnvelope::ok(ChunkedStatusResponse {
            received: vec![Range { offset: 0, length: total }],
            missing: vec![],
            received_bytes: total,
            total_size: total,
            completed: true,
        }));
    }

    let (part_size, total_parts, reference) = s3_reference_of(session.manifest())?;
    let backend = numbered_backend(state)?;

    match backend.list_parts(&reference).await {
        Ok(parts) => {
            let received = ranges_from_parts(&parts, part_size, total_parts, total);
            let missing = missing_complement(&received, total);
            let received_bytes: u64 = received.iter().map(|r| r.length).sum();
            Ok(ApiEnvelope::ok(ChunkedStatusResponse {
                received,
                missing,
                received_bytes,
                total_size: total,
                completed: false,
            }))
        }
        Err(NumberedPartError::NoSuchUpload) => {
            // MPU 已关闭：🔴 status 只报告字节状态，不建行/不写墓碑/不做删除
            // （建行需要 complete 体里的 CEK/business_id/encryption_version）。
            let probe = object_probe(state)?;
            match probe.head(&reference).await {
                Err(e) => Err(probe_err(e, "HEAD final key 失败")),
                Ok(None) => Err(coded(
                    E::UploadSessionGone,
                    410,
                    "分片上传已被关闭，请重新申请 token 从头上传",
                )),
                Ok(Some(head)) => {
                    if head.privchat_upload_id.as_deref() != Some(session.upload_id()) {
                        tracing::error!(
                            "final key 上对象 metadata 不属于会话 {}，保留对象人工排查",
                            session.upload_id()
                        );
                        return Err(ServerError::Internal(
                            "final key 上存在不属于当前会话的对象，已保留，请人工排查".to_string(),
                        ));
                    }
                    if head.content_length == total {
                        // 字节已齐：照常 complete（带 CEK）走 §8.5 第 3 步真正恢复。
                        Ok(ApiEnvelope::ok(ChunkedStatusResponse {
                            received: vec![Range { offset: 0, length: total }],
                            missing: vec![],
                            received_bytes: total,
                            total_size: total,
                            completed: false,
                        }))
                    } else {
                        // 🔴 长度不符不得把明显不完整的对象报告成完整。
                        tracing::warn!(
                            "final key 对象长度 {} 与声明 {total} 不符（会话 {}），回可重试错误",
                            head.content_length,
                            session.upload_id()
                        );
                        Err(ServerError::Internal(format!(
                            "final key 上对象长度 {} 与声明 {total} 不符，不能报告为完整，请重试",
                            head.content_length
                        )))
                    }
                }
            }
        }
        Err(e) => Err(list_err(&e, "ListParts 失败")),
    }
}

/// `POST /files/complete` 的 S3 分支（§8.5，全程持锁——锁由调用方持有）。
pub(super) async fn s3_complete(
    state: &FileServerState,
    session: &ChunkedSession,
    extra: CompleteRequest,
    headers: &axum::http::HeaderMap,
) -> ApiResult<UploadResponse> {
    // 1. 墓碑 → 幂等回原 file_id（与 proxy 相同）。
    if let Some(existing) = session.completed_file_id()? {
        return chunked_completed_response(state, session, existing).await;
    }

    // 2. 预留 id 已落库？（PG 提交后、墓碑前崩溃的恢复路径，与 proxy 相同）
    let reserved = session.manifest().reserved_file_id;
    if let Some(meta) = state.file_service.get_file_metadata(reserved).await? {
        if !manifest_matches(session, &meta) {
            return Err(ServerError::Internal(format!(
                "预留的 file_id={reserved} 已被另一份内容占用，拒绝继续"
            )));
        }
        tracing::info!("♻️ 预留的 file_id={reserved} 已落库，补写墓碑并返回");
        session.write_completed(reserved)?;
        session.drop_payload();
        return chunked_completed_response(state, session, reserved).await;
    }

    let (part_size, total_parts, reference) = s3_reference_of(session.manifest())?;
    let backend = numbered_backend(state)?;
    let probe = object_probe(state)?;

    // 3. 🔴 HEAD final_key（在 ListParts 之前，未做任何不可逆操作）。
    if let Some(head) = probe
        .head(&reference)
        .await
        .map_err(|e| probe_err(e, "HEAD final key 失败"))?
    {
        return recover_from_existing_object(
            state, session, &extra, headers, backend, probe, &reference, head,
        )
        .await;
    }

    // 缺片预检 + 权威快照：一次 ListParts 两用。
    let snapshot = match backend.list_parts(&reference).await {
        Ok(parts) => parts,
        Err(NumberedPartError::NoSuchUpload) => {
            // 列片前 HEAD 还是空的：MPU 在两者之间被关闭且没发布 → 会话作废。
            // （若已发布，HEAD 会在下面命中并走三分支。）
            return match probe
                .head(&reference)
                .await
                .map_err(|e| probe_err(e, "HEAD final key 失败"))?
            {
                Some(head) => {
                    recover_from_existing_object(
                        state, session, &extra, headers, backend, probe, &reference, head,
                    )
                    .await
                }
                None => Err(restart_required("分片上传已被关闭，请重新申请 token 从头上传")),
            };
        }
        Err(e) => return Err(list_err(&e, "ListParts 失败")),
    };

    // 4. 🔴 第二十九轮（COS 最小兼容）：分片身份证据 = 几何 + 本地声明。
    // part_number/ETag/size 从 ListParts 回读；每片 checksum 取自 manifest 声明（part-url 签发时
    // 持久化）——COS 的 ListParts 不回逐片摘要，不再从 S3 回读。
    // 几何异常或缺 manifest 声明的片按缺失处理 → 409 回缺失区间，会话保持可补片。
    let manifest = session.manifest();
    let mut completed_parts: Vec<CompletedPart> = Vec::new();
    for p in &snapshot {
        let geometry_ok =
            check_part_geometry(p.part_number, p.size, total_parts, part_size, manifest.total_size)
                .is_ok();
        if let (true, Some(checksum)) = (geometry_ok, manifest.part_digests.get(&p.part_number)) {
            if !completed_parts.iter().any(|c| c.part_number == p.part_number) {
                completed_parts.push(CompletedPart {
                    part_number: p.part_number,
                    etag: p.etag.clone(),
                    checksum_sha256_b64: checksum.clone(),
                });
            }
        }
    }
    if completed_parts.len() != total_parts as usize {
        let received = ranges_from_parts(&snapshot, part_size, total_parts, manifest.total_size);
        let missing = missing_complement(&received, manifest.total_size);
        let received_bytes: u64 = received.iter().map(|r| r.length).sum();
        tracing::info!("S3 complete 缺片预检未过：missing={missing:?}");
        return Err(coded(
            E::UploadMissingRanges,
            409,
            format!(
                "还有区间没传完（已收 {received_bytes} / {} 字节），请 GET status 补齐",
                manifest.total_size
            ),
        ));
    }
    completed_parts.sort_by_key(|c| c.part_number);

    // 5. CompleteMultipartUpload（If-None-Match: * 由 backend 强制携带；第二十九轮起它不再是
    // 必备安全闸门，不可覆盖性由 final_key 唯一性 + HEAD 预检 + 整文件回读保障）。
    if let Err(e) = backend.complete(&reference, &completed_parts).await {
        // 🔴 恢复动作取自 complete_recovery_for（冻结语义唯一真源），不按 HTTP 码猜。
        return match complete_recovery_for(&e) {
            Some(CompleteRecovery::RestartUpload) => {
                Err(restart_required("分片上传已作废（409），请重新申请 token 从头上传"))
            }
            Some(CompleteRecovery::VerifyExistingObject) => {
                verify_precondition_failed(state, session, &extra, headers, backend, probe, &reference)
                    .await
            }
            None => match e {
                NumberedPartError::NoSuchUpload => match probe
                    .head(&reference)
                    .await
                    .map_err(|e| probe_err(e, "HEAD final key 失败"))?
                {
                    Some(head) => {
                        recover_from_existing_object(
                            state, session, &extra, headers, backend, probe, &reference, head,
                        )
                        .await
                    }
                    None => Err(restart_required("分片上传已被关闭，请重新申请 token 从头上传")),
                },
                NumberedPartError::Backend(m) => {
                    Err(ServerError::Internal(format!("组装分片失败: {m}")))
                }
                // Conflict/PreconditionFailed 已被 complete_recovery_for 分走。
                _ => unreachable!("complete_recovery_for 未覆盖的错误"),
            },
        };
    }

    // 6. 🔴 流式回读 + 解密重算：文件身份的唯一权威。
    //
    // multipart 的 SHA-256 是 composite，不等于整文件摘要；而**密文摘要本身也证明
    // 不了身份**——每块用新随机 nonce，同一份明文封装两次得到不同密文，摘要对得上
    // 只说明字节没坏。只有解出明文重算摘要，才能挡住「声明 A 的身份 + 上传 B 的密文」。
    let verified = match verify_final_object(state, probe.as_ref(), &reference, session.manifest())
        .await
    {
        Ok(v) => v,
        // 内容不符是终局的：本次 Complete 刚组装的对象本该携带本会话 metadata，
        // 满足统一删除规则。
        //
        // 🔴 但删除前**必须重新证明归属**，不能只靠"这是我刚组装的"。校验读流与这次
        // HEAD 之间对象可以被替换：ETag 条件删除只保证「删的是我 HEAD 到的那一个」，
        // 保证不了「那一个是我的」——少了这一步，一个被替换进来的、别人的对象会被
        // 条件删除干净利落地删掉。归属核对与条件删除必须一起用，缺一不可。
        Err(ServerError::Validation(_)) => {
            return match probe
                .head(&reference)
                .await
                .map_err(|e| probe_err(e, "HEAD final key 失败"))?
            {
                // 对象已被并发清除：等同删除成功，让客户端从头来。
                None => Err(restart_required(
                    "回读校验与声明不符且对象已不存在，请重新申请 token 从头上传",
                )),
                Some(head)
                    if head.privchat_upload_id.as_deref() == Some(session.upload_id()) =>
                {
                    delete_then_restart_or_retry(probe, &reference, "回读校验与声明不符", &head.etag)
                        .await
                }
                // 归属证明不了 → 一律不删（口径同 §8.5 第 3 步分支一）：保留对象、
                // abort 当前 MPU、报冲突等人工排查。🔴 不得回 20618——重申请仍是同一
                // final_key，会死循环。
                Some(_) => {
                    must_abort(backend, &reference).await?;
                    tracing::error!(
                        "回读校验与声明不符，但 final key 上的对象 metadata 不属于会话 {}，保留对象人工排查",
                        session.upload_id()
                    );
                    Err(ServerError::Internal(
                        "final key 上存在不属于当前会话的对象，拒绝继续，请人工排查".to_string(),
                    ))
                }
            };
        }
        // 存储读取故障（可重试）：🔴 绝不能顺手删对象——字节可能好好地躺在桶里，
        // 删掉等于把一次网络抖动变成用户的重传。
        Err(e) => return Err(e),
    };

    // 7. 建行（对象已在 final 路径）→ 墓碑 → 删本地 parts（本就为空）。
    let metadata =
        record_and_finish(state, session, &extra, headers, probe, &reference, verified.sealed_sha256)
            .await?;
    tracing::info!(
        "✅ S3 直传完成: file_id={} upload_id={}",
        metadata.file_id,
        session.upload_id()
    );
    Ok(ApiEnvelope::ok(upload_response_of(state, metadata)))
}

/// §8.5 第 3 步 HEAD 三分支。🔴 三个分支不得合并处理，否则删除失败后的重试
/// 永远无法自愈。
async fn recover_from_existing_object(
    state: &FileServerState,
    session: &ChunkedSession,
    extra: &CompleteRequest,
    headers: &axum::http::HeaderMap,
    backend: &Arc<dyn NumberedPartBackend>,
    probe: &Arc<dyn FinalObjectProbe>,
    reference: &UploadReference,
    head: FinalObjectHead,
) -> ApiResult<UploadResponse> {
    // 分支一：metadata 不属于当前 session → 保留对象（无权删除）、abort 当前
    // MPU、报内部冲突。🔴 不得回 20618——重申请仍是同一 final_key，会死循环。
    if head.privchat_upload_id.as_deref() != Some(session.upload_id()) {
        must_abort(backend, reference).await?;
        tracing::error!(
            "final key 上已有对象但 metadata 不属于会话 {}，保留对象人工排查",
            session.upload_id()
        );
        return Err(ServerError::Internal(
            "final key 上存在不属于当前会话的对象，拒绝继续，请人工排查".to_string(),
        ));
    }

    // 🔴 这条恢复路径也必须真校验。"对象是本会话留下的"只说明谁写的，不说明写的是
    // 什么——跳过校验就等于给这条路留了一个完整的绕过入口。
    match verify_final_object(state, probe.as_ref(), reference, session.manifest()).await {
        Ok(verified) => {
            // 分支二：属于本 session + 身份一致 → 补建行 + 墓碑（幂等 abort 当前 MPU；
            // 🔴 abort 失败回可重试 5xx，绝不带着存活的 MPU 建行）。
            must_abort(backend, reference).await?;
            let metadata = record_and_finish(
                state,
                session,
                extra,
                headers,
                probe,
                reference,
                verified.sealed_sha256,
            )
            .await?;
            tracing::info!(
                "♻️ final key 已有本会话对象且校验一致，补建行: file_id={}",
                metadata.file_id
            );
            Ok(ApiEnvelope::ok(upload_response_of(state, metadata)))
        }
        // 分支三：属于本 session + 内容不符（上一轮校验不过但删除失败的重试）→
        // 满足统一删除规则，允许删：成功 → 20618；失败/拒绝 → 保留会话、可重试 5xx。
        Err(ServerError::Validation(_)) => {
            delete_then_restart_or_retry(
                probe,
                reference,
                "final key 上对象与本次上传的声明不符",
                &head.etag,
            )
            .await
        }
        // 读取故障：可重试，绝不因此删对象。
        Err(e) => Err(e),
    }
}

/// §8.5 第 5 步 412（PreconditionFailed）恢复：final key 已有对象、本次 MPU
/// 未发布。🔴 绝不删除 final key——key 上的是此前已存在的对象。
async fn verify_precondition_failed(
    state: &FileServerState,
    session: &ChunkedSession,
    extra: &CompleteRequest,
    headers: &axum::http::HeaderMap,
    backend: &Arc<dyn NumberedPartBackend>,
    probe: &Arc<dyn FinalObjectProbe>,
    reference: &UploadReference,
) -> ApiResult<UploadResponse> {
    match probe
        .head(reference)
        .await
        .map_err(|e| probe_err(e, "HEAD final key 失败"))?
    {
        None => {
            // 412 说明当时存在、HEAD 却读不到：竞态窗口，回可重试错误。
            Err(ServerError::Internal(
                "complete 时 final key 已存在（412）但随后读不到，请重试".to_string(),
            ))
        }
        Some(head) => {
            let ours = head.privchat_upload_id.as_deref() == Some(session.upload_id());
            if ours {
                // 🔴 复用已有对象前必须真校验：这条路径同样是一条发布路径，
                // 归属核对只回答"谁写的"，回答不了"写的是不是 token 声明的那份"。
                match verify_final_object(state, probe.as_ref(), reference, session.manifest())
                    .await
                {
                    Ok(verified) => {
                        // 身份一致 → 复用继续建行；🔴 建行前先幂等 abort 当前 MPU，
                        // abort 失败回可重试 5xx，绝不带着存活的 MPU 建行。
                        must_abort(backend, reference).await?;
                        let metadata = record_and_finish(
                            state,
                            session,
                            extra,
                            headers,
                            probe,
                            reference,
                            verified.sealed_sha256,
                        )
                        .await?;
                        tracing::info!(
                            "♻️ 412 后核验身份一致，复用已有对象建行: file_id={}",
                            metadata.file_id
                        );
                        return Ok(ApiEnvelope::ok(upload_response_of(state, metadata)));
                    }
                    // 内容不符 → 落到下面「保留对象、abort、人工排查」。
                    Err(ServerError::Validation(_)) => {}
                    // 读取故障是可重试的：🔴 不能因为读不动就判成"身份不符"，
                    // 那会把一次抖动升级成需要人工排查的冲突。
                    Err(e) => return Err(e),
                }
            }
            // 身份不一致 → 保留已有对象、abort 当前 MPU、报内部冲突。
            must_abort(backend, reference).await?;
            tracing::error!(
                "412 后 final key 对象与本次上传身份不符（ours={ours}），保留对象人工排查",
            );
            Err(ServerError::Internal(
                "final key 上的对象与本次上传不一致，已保留，请人工排查".to_string(),
            ))
        }
    }
}

/// 归属已证明的删除：成功 → 20618 从零重来；失败/拒绝 → 保留会话、可重试
/// 5xx（24h 扫描器继续删，重试可自愈）。🔴 删除以 HEAD 的 ETag 为条件
/// （delete_if_match），检查与删除之间对象被替换时拒绝删除而不是删错对象。
async fn delete_then_restart_or_retry(
    probe: &Arc<dyn FinalObjectProbe>,
    reference: &UploadReference,
    reason: &str,
    etag: &str,
) -> ApiResult<UploadResponse> {
    match probe.delete_if_match(reference, etag).await {
        Ok(true) => Err(restart_required(format!(
            "{reason}，已删除该对象，请重新申请 token 从头上传"
        ))),
        Ok(false) => {
            tracing::error!("{reason}且删除前对象已变化（ETag 不匹配），拒绝删除，会话保留等待重试");
            Err(ServerError::Internal(format!(
                "{reason}且对象在删除前已变化，拒绝删除，请稍后重试"
            )))
        }
        Err(e) => {
            tracing::error!("{reason}且删除 final 对象失败（{}），会话保留等待重试", e);
            Err(ServerError::Internal(format!(
                "{reason}且删除 final 对象失败，请稍后重试: {e}"
            )))
        }
    }
}

/// §8.5 第 7 步：建行（含秒传收敛）→ 原子写墓碑 → 删本地 parts。
/// 墓碑守正常路径，预留 id 守崩溃窗口（顺序同 §3.3）。
async fn record_and_finish(
    state: &FileServerState,
    session: &ChunkedSession,
    extra: &CompleteRequest,
    headers: &axum::http::HeaderMap,
    probe: &Arc<dyn FinalObjectProbe>,
    reference: &UploadReference,
    stored_sha256: String,
) -> Result<crate::service::FileMetadata, ServerError> {
    let fields = || crate::service::file_service::RecordFields {
        filename: session.manifest().filename.clone(),
        mime_type: session.manifest().mime_type.clone(),
        uploader_id: session.manifest().uploader_id,
        uploader_ip: client_ip_from_headers(headers),
        business_type: session.manifest().business_type.clone(),
        business_id: extra.business_id.clone(),
        // 🔴 身份取自 manifest（= token 冻结的那份），不取 complete 请求体。
        plaintext_sha256: session.manifest().plaintext_sha256.clone(),
        plaintext_size: session.manifest().plaintext_size,
        format_version: session.manifest().format_version,
        encryption_key_id: session.manifest().encryption_key_id,
        chunk_plain_size: session.manifest().chunk_plain_size,
        // 冻结的密文长度 = 建会话时签下的 total_size。
        sealed_size: session.manifest().total_size,
    };
    use crate::service::file_service::S3RecordOutcome;
    let metadata = match state
        .file_service
        .record_s3_published(session, stored_sha256.clone(), fields(), false)
        .await?
    {
        S3RecordOutcome::Recorded(meta) => meta,
        S3RecordOutcome::DuplicateObject => {
            // 秒传命中：本次刚发布的 final 对象冗余。它由本会话 MPU 组装（metadata 属于
            // 本会话，final_key 由预留 file_id 生成不会撞别人）→ 满足统一删除规则；删除后
            // 用既有路径建行。
            match probe
                .head(reference)
                .await
                .map_err(|e| probe_err(e, "HEAD final key 失败"))?
            {
                Some(head) if head.privchat_upload_id.as_deref() == Some(session.upload_id()) => {
                    // 🔴 条件删除：以 HEAD 的 ETag 为准，对象已变化即拒绝。
                    match probe
                        .delete_if_match(reference, &head.etag)
                        .await
                        .map_err(|e| probe_err(e, "秒传命中但删除冗余 final 对象失败"))?
                    {
                        true => {}
                        false => {
                            return Err(ServerError::Internal(
                                "秒传命中但 final 对象在删除前已变化，拒绝删除，请重试"
                                    .to_string(),
                            ))
                        }
                    }
                }
                _ => {
                    return Err(ServerError::Internal(
                        "秒传命中但 final 对象归属无法证明，拒绝删除".to_string(),
                    ))
                }
            }
            match state
                .file_service
                .record_s3_published(session, stored_sha256, fields(), true)
                .await?
            {
                S3RecordOutcome::Recorded(meta) => meta,
                S3RecordOutcome::DuplicateObject => {
                    return Err(ServerError::Internal(
                        "秒传收敛第二次仍判重，不应发生".to_string(),
                    ))
                }
            }
        }
    };
    // 墓碑（原子 + fsync）：落库成功后写不上只影响下次重试走第 2 步，不判失败。
    if let Err(e) = session.write_completed(metadata.file_id) {
        tracing::warn!("写 S3 完成墓碑失败 file_id={}: {e}", metadata.file_id);
    } else {
        // 墓碑之后才删本地 parts（S3 会话本就为空，口径与 proxy 一致）。
        session.drop_payload();
    }
    Ok(metadata)
}

/// `POST /files/abort` 的 S3 分支（§8.3）。🔴 顺序冻结：先 S3 abort +
/// ListParts 确认清空，**才删本地目录**；S3 调用失败会话保持可重试，绝不先
/// 置本地终态。
pub(super) async fn s3_abort(
    state: &FileServerState,
    session: &ChunkedSession,
) -> ApiResult<serde_json::Value> {
    let (_, _, reference) = s3_reference_of(session.manifest())?;
    let backend = numbered_backend(state)?;

    // AbortMultipartUpload（NoSuchUpload 视为成功）。
    match backend.abort(&reference).await {
        Ok(()) | Err(NumberedPartError::NoSuchUpload) => {}
        Err(e) => {
            return Err(ServerError::Internal(format!(
                "中止分片上传失败，会话保留，请稍后重试: {e:?}"
            )))
        }
    }

    // 🔴 用 ListParts 确认清理：返回空或 NoSuchUpload 都算成功；仍有 part 则
    // 继续 abort（in-flight UploadPart 可能在 abort 后仍写入成功）。
    let mut confirmed = false;
    for attempt in 0..3u32 {
        if attempt > 0 {
            match backend.abort(&reference).await {
                Ok(()) | Err(NumberedPartError::NoSuchUpload) => {}
                Err(e) => {
                    return Err(ServerError::Internal(format!(
                        "重试中止分片上传失败，会话保留，请稍后重试: {e:?}"
                    )))
                }
            }
        }
        match backend.list_parts(&reference).await {
            Ok(parts) if parts.is_empty() => {
                confirmed = true;
                break;
            }
            Ok(_) => continue,
            Err(NumberedPartError::NoSuchUpload) => {
                confirmed = true;
                break;
            }
            Err(e) => {
                return Err(ServerError::Internal(format!(
                    "确认分片清理失败，会话保留，请稍后重试: {e:?}"
                )))
            }
        }
    }
    if !confirmed {
        return Err(ServerError::Internal(
            "分片上传多次中止后仍有残留 part，会话保留，请稍后重试".to_string(),
        ));
    }

    // 确认清空后才删本地目录。
    session.discard()?;
    Ok(ApiEnvelope::ok(serde_json::json!({ "aborted": true })))
}

/// S3 三条发布路径共用的首传校验。
///
/// 🔴 **一次 GET 同时拿长度和字节**，不先 HEAD 再 GET：两者之间对象可以被替换，
/// 而"长度已核过"正是 `verify_attachment` 把后续 IO 失败判成可重试的前提。
/// 也不先 `sha256_of()` 再 GET 一遍——密文摘要和明文摘要在同一趟里一起算出来。
///
/// 🔴 这三条路径（正常 complete、已存在对象恢复、412 恢复）**必须都走这里**。
/// 漏掉任何一条，那条就是完整的绕过入口：密文摘要对得上只说明字节没坏，
/// 说明不了"这串字节就是 token 声明的那份内容"。
/// 🔴 密钥缺席时**拒绝**，不是跳过校验：没有密钥就重算不出明文身份，
/// 而"没校验"和"校验过了"绝不能是同一个结果。这是配置问题（可修复后重试），
/// 不是客户端的内容错误，所以回可重试的 5xx 而不是 400。
async fn verify_final_object(
    state: &FileServerState,
    probe: &dyn crate::service::final_object_probe::FinalObjectProbe,
    reference: &crate::service::final_object_probe::UploadReference,
    manifest: &crate::service::chunked_upload::Manifest,
) -> Result<crate::service::attachment_verify::VerifiedAttachment, ServerError> {
    let site_key = super::upload::site_key_of(state, manifest.encryption_key_id)?;

    // 🔴 建流失败（连不上、超时、缺 Content-Length）与「读到一半断线」是同一类事：
    // 字节可能好好地躺在桶里，只是这一次没读成。必须回 `ServiceUnavailable`——
    // `probe_err` 给的是 `Internal`，而 SDK 的可重试白名单并不覆盖所有 Internal，
    // 一次抖动就会变成客户端眼里的终局失败。
    let (observed_size, reader) = probe.open_stream(reference).await.map_err(|e| {
        tracing::warn!(error = %e, "打开 final 对象回读流失败（可重试）");
        ServerError::ServiceUnavailable("读取待校验附件失败".to_string())
    })?;

    crate::service::attachment_verify::verify_attachment(
        reader,
        observed_size,
        &crate::service::attachment_verify::FrozenIdentity {
            plaintext_sha256: manifest.plaintext_sha256.clone(),
            plaintext_size: manifest.plaintext_size,
            sealed_size: manifest.total_size,
            format_version: manifest.format_version,
            encryption_key_id: manifest.encryption_key_id,
            chunk_plain_size: manifest.chunk_plain_size,
        },
        &site_key,
    )
    .await
}
