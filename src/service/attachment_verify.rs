// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Licensed under the Apache License, Version 2.0 (the "License").

//! 首传校验：**每一条**发布路径的唯一入口。
//!
//! 🔴 密文摘要证明不了身份。客户端每块都用新的随机 nonce，同一份明文封装两次得到不同
//! 密文——所以"密文摘要对得上"只说明字节没在传输中坏掉，不说明"这串字节就是你声明的
//! 那份内容"。不解密重算的话，任何登录用户都能提交
//! 「文件 A 的明文摘要 + 文件 B 的合法密文」，把 A 的秒传结果污染成 B。
//!
//! 🔴 **整文件、proxy complete、S3 complete、S3 已存在对象恢复、S3 412 恢复，五条路径
//! 必须全部走这里。** 漏掉任何一条，那条就是完整的绕过入口——上一版只在整文件路径上放了
//! 守卫，另外四条照样能发布未验证对象，而"已经 fail-closed"的说法掩盖了这件事。
//!
//! 存储后端替不了这件事：它没有密钥，也不该有。

use privchat_protocol::attachment_crypto as ac;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{Result, ServerError};

/// token 冻结的身份。complete 只**核对**它，不从请求体里读任何一项。
#[derive(Debug, Clone)]
pub struct FrozenIdentity {
    pub plaintext_sha256: String,
    pub plaintext_size: u64,
    pub sealed_size: u64,
    pub format_version: u8,
    pub encryption_key_id: u8,
    pub chunk_plain_size: u32,
}

/// 校验通过之后服务端才有的事实。
#[derive(Debug, Clone)]
pub struct VerifiedAttachment {
    /// 服务端对**实际落盘字节**算出的 SHA-256。客户端声明的同名值只作预检。
    pub sealed_sha256: String,
}

/// 对外**只有一句话**，细节只进日志。
///
/// 🔴 上一版把 protocol 的错误直接 `map_err` 出去，于是「不是附件 blob」
/// 「格式版本不认识」「认证失败」是三句不同的话——那正好给攻击者一个逐项试探的信道，
/// 而当时的测试只覆盖了碰巧相同的三种失败，没发现这件事。
///
/// 所以细节在这里就地丢弃，只留给服务端日志。
fn reject(detail: impl AsRef<str>) -> ServerError {
    tracing::warn!(reason = detail.as_ref(), "附件首传校验失败");
    ServerError::Validation(REJECTION.to_string())
}

/// 唯一对外可见的失败措辞。
pub const REJECTION: &str = "附件校验失败";

/// 读取密文时的 IO 失败**一律**是可重试的存储故障，不是内容错误。
///
/// 🔴 把 COS 连接中断、超时、磁盘读失败映射成 `Validation`，等于给客户端一个终局
/// 400：一次网络抖动让这次上传永久失败，而字节其实好好地躺在桶里。
///
/// 🔴 **`UnexpectedEof` 也在此列。** 上一版把它单独判成"对象截断"，那是在猜：
/// `AsyncRead` 分不清"对象本身就短"和"响应流提前结束"——HEAD 显示长度正确、
/// 回读中途断开同样表现为 EOF，于是一次抖动照样变成终局 400。
///
/// 对象完不完整由**权威长度**回答（`observed_size`，见 `verify_attachment`），
/// 在读第一个字节之前就判掉。到了这里长度已经核过，任何 IO 失败都只能是传输问题。
fn io_failure(context: &str, e: std::io::Error) -> ServerError {
    tracing::warn!(context, error = %e, "读取待校验密文失败（可重试）");
    ServerError::ServiceUnavailable(format!("读取待校验附件失败: {context}"))
}

/// 流式读密文、解密、重算明文身份。
///
/// `reader` 交出的必须是**完整的**密文对象。函数自己不做任何 IO 之外的假设：
/// 谁调用它、字节从本地临时文件还是从 S3 回读来，都不影响判据。
/// 🔴 **必须是流式的，而且是异步的。**
///
/// 同步 `Read` 只够读本地临时文件；S3 回读是异步响应流，在 Tokio worker 上做阻塞
/// 网络读会拖垮整个 runtime。而"先整份读进内存再验"更不行——校验的内存占用不能
/// 随文件大小走，那是这套分块格式一开始就要避免的事。
/// `observed_size` 是**权威长度**，必须在读第一个字节之前就与冻结的 `sealed_size`
/// 比对——这是唯一能把"对象本身不完整"和"读到一半断线"分开的地方，`AsyncRead`
/// 自己做不到。
///
/// 🔴 **S3 必须取自交出 `reader` 的那一次 GET 响应的 `Content-Length`，不能用之前
/// 单独做的 HEAD。** HEAD 与 GET 之间对象可以被改掉：拿 HEAD 的长度去核 GET 的字节，
/// 核的是两个不同时刻的东西，而"长度已经核过"正是下面把所有 IO 失败判成可重试的前提。
/// 长度和字节必须来自同一次响应。
///
/// 本地对象取打开文件后的 metadata 长度（同一个 fd，没有这个窗口）。
pub async fn verify_attachment<R: AsyncRead + Unpin>(
    mut reader: R,
    observed_size: u64,
    frozen: &FrozenIdentity,
    site_key: &[u8],
) -> Result<VerifiedAttachment> {
    // 密文大小是 token 冻结的，先按它挡掉明显不对的输入——不然下面要按一个错误的
    // 块数去循环。
    let expected_chunks = ac::chunk_count_for(frozen.plaintext_size, frozen.chunk_plain_size)
        .map_err(reject)?;
    let expected_sealed = ac::sealed_len(frozen.plaintext_size, frozen.chunk_plain_size)
        .map_err(reject)?;
    if expected_sealed != frozen.sealed_size {
        return Err(reject("sealed size does not match the frozen identity"));
    }
    // 🔴 长度先于一切。对象短了/长了都是内容问题，终局拒绝；这一关过了之后，
    // 读取过程中的任何失败就只可能是传输问题（可重试），不必再去猜 EOF 的来源。
    if observed_size != frozen.sealed_size {
        return Err(reject("stored object size does not match the frozen identity"));
    }

    let mut sealed_hasher = Sha256::new();
    let mut plain_hasher = Sha256::new();

    let mut header_bytes = [0u8; ac::HEADER_LEN];
    reader
        .read_exact(&mut header_bytes)
        .await
        .map_err(|e| io_failure("read header", e))?;
    sealed_hasher.update(header_bytes);

    let header = ac::AttachmentHeader::parse(&header_bytes).map_err(reject)?;

    // 🔴 header 里的加密参数必须与 token 冻结的一致。
    //
    // 不核对的话，客户端可以拿一张为 1 MiB 块签发的 token 上传 4 KiB 块的对象：
    // 解密照样成功（header 自描述），但它跟 token 承诺的不是同一份东西，而
    // `sealed_size` 的比对也已经被它自己的 header 绕开了。
    if header.encryption_key_id != frozen.encryption_key_id
        || header.chunk_plain_size != frozen.chunk_plain_size
        || header.plaintext_size != frozen.plaintext_size
        || header.chunk_count != expected_chunks
    {
        return Err(reject("header disagrees with the frozen identity"));
    }
    // format_version 由 `AttachmentHeader::parse` 保证是当前版本；这里再核一次
    // token 冻结的那个值，防止两边将来分头演进。
    if frozen.format_version != ac::FORMAT_VERSION {
        return Err(reject("frozen format version is not the current one"));
    }

    let mut opener = ac::AttachmentOpener::new(&header_bytes, site_key).map_err(reject)?;
    let mut plaintext_total: u64 = 0;

    for index in 0..expected_chunks {
        let plain_len = ac::expected_chunk_len(&header, index).map_err(reject)?;
        let mut chunk = vec![0u8; ac::NONCE_LEN + 4 + plain_len as usize + ac::TAG_LEN];
        reader
            .read_exact(&mut chunk)
            .await
            .map_err(|e| io_failure(&format!("read chunk {index}"), e))?;
        sealed_hasher.update(&chunk);

        let plain = opener.open_chunk(&chunk).map_err(reject)?;
        plaintext_total += plain.len() as u64;
        plain_hasher.update(&plain);
        // 明文用完即弃，不在内存里攒整份——大文件校验不能按文件大小吃内存。
        drop(plain);
    }

    // 多出来的字节说明这不是一个规规矩矩的对象；哪怕每块都验过也不能收。
    let mut trailing = [0u8; 1];
    if reader
        .read(&mut trailing)
        .await
        .map_err(|e| io_failure("read trailer", e))?
        != 0
    {
        return Err(reject("object has trailing bytes"));
    }

    // 块数与累计明文长度的兜底（见 protocol 里 `finish` 的说明）。
    opener.finish().map_err(reject)?;
    if plaintext_total != frozen.plaintext_size {
        return Err(reject("plaintext length does not match the frozen identity"));
    }

    // 🔴 这一步才是身份判据：解出来的明文摘要必须等于 token 冻结的那个。
    // 「A 的摘要 + B 的密文」在这里被挡住。
    let plaintext_sha256 = hex::encode(plain_hasher.finalize());
    if !plaintext_sha256.eq_ignore_ascii_case(&frozen.plaintext_sha256) {
        return Err(reject("plaintext digest does not match the frozen identity"));
    }

    Ok(VerifiedAttachment {
        sealed_sha256: hex::encode(sealed_hasher.finalize()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        [9u8; 32]
    }

    fn frozen_for(plain: &[u8], chunk: u32) -> FrozenIdentity {
        FrozenIdentity {
            plaintext_sha256: hex::encode(Sha256::digest(plain)),
            plaintext_size: plain.len() as u64,
            sealed_size: ac::sealed_len(plain.len() as u64, chunk).unwrap(),
            format_version: ac::FORMAT_VERSION,
            encryption_key_id: 1,
            chunk_plain_size: chunk,
        }
    }

    const CHUNK: u32 = ac::MIN_CHUNK_PLAIN_SIZE;

    fn sealed(plain: &[u8]) -> Vec<u8> {
        ac::encrypt_attachment_with_chunk_size(plain, &key(), 1, CHUNK).unwrap()
    }

    #[tokio::test]
    async fn a_matching_upload_passes_and_reports_the_server_computed_digest() {
        let plain = vec![7u8; (2 * CHUNK + 100) as usize];
        let blob = sealed(&plain);
        let v = verify_attachment(blob.as_slice(), blob.len() as u64, &frozen_for(&plain, CHUNK), &key())
            .await
            .expect("正常上传必须通过");
        assert_eq!(v.sealed_sha256, hex::encode(Sha256::digest(&blob)));
    }

    /// 🔴 这条是整个 complete 校验存在的理由：声明 A 的摘要、上传 B 的密文。
    ///
    /// 两份密文各自都完全合法，密文摘要也各自自洽——只有解密重算才拦得住。
    #[tokio::test]
    async fn declaring_one_file_and_uploading_another_is_rejected() {
        let a = vec![1u8; (2 * CHUNK) as usize];
        let b = vec![2u8; (2 * CHUNK) as usize];
        let blob_b = sealed(&b);
        let mut frozen = frozen_for(&a, CHUNK);
        // 两者等长，所以 sealed_size 也一样——尺寸检查放行，身份检查必须拦下。
        frozen.sealed_size = ac::sealed_len(b.len() as u64, CHUNK).unwrap();
        assert!(verify_attachment(blob_b.as_slice(), blob_b.len() as u64, &frozen, &key()).await.is_err());
    }

    #[tokio::test]
    async fn a_wrong_site_key_is_rejected() {
        let plain = vec![3u8; 100];
        let blob = sealed(&plain);
        assert!(verify_attachment(blob.as_slice(), blob.len() as u64, &frozen_for(&plain, CHUNK), &[8u8; 32]).await.is_err());
    }

    /// header 声明的分块几何必须与 token 冻结的一致。解密照样会成功
    /// （header 是自描述的），但那不是 token 承诺的那份东西。
    #[tokio::test]
    async fn a_chunk_geometry_that_differs_from_the_token_is_rejected() {
        let plain = vec![4u8; (3 * CHUNK) as usize];
        let blob = ac::encrypt_attachment_with_chunk_size(&plain, &key(), 1, CHUNK * 2).unwrap();
        // 用 CHUNK 冻结，实际按 CHUNK*2 封装。
        assert!(verify_attachment(blob.as_slice(), blob.len() as u64, &frozen_for(&plain, CHUNK), &key()).await.is_err());
    }

    #[tokio::test]
    async fn a_wrong_key_id_in_the_header_is_rejected() {
        let plain = vec![5u8; 100];
        let blob = ac::encrypt_attachment_with_chunk_size(&plain, &key(), 2, CHUNK).unwrap();
        // token 冻结的是 key_id=1。
        assert!(verify_attachment(blob.as_slice(), blob.len() as u64, &frozen_for(&plain, CHUNK), &key()).await.is_err());
    }

    #[tokio::test]
    async fn truncated_and_padded_objects_are_both_rejected() {
        let plain = vec![6u8; (2 * CHUNK) as usize];
        let frozen = frozen_for(&plain, CHUNK);
        let blob = sealed(&plain);

        let mut short = blob.clone();
        short.truncate(short.len() - 1);
        assert!(verify_attachment(short.as_slice(), short.len() as u64, &frozen, &key()).await.is_err());

        let mut long = blob.clone();
        long.push(0);
        assert!(verify_attachment(long.as_slice(), long.len() as u64, &frozen, &key()).await.is_err());
    }

    /// 失败信息不能区分"哪一项对不上"——分开说等于给攻击者一个逐项试探的信道。
    #[tokio::test]
    async fn every_rejection_looks_the_same_from_outside() {
        let plain = vec![7u8; 100];
        let frozen = frozen_for(&plain, CHUNK);
        let mut messages = std::collections::HashSet::new();
        for blob in [
            sealed(&vec![8u8; 100]),
            ac::encrypt_attachment_with_chunk_size(&plain, &key(), 2, CHUNK).unwrap(),
            {
                let mut b = sealed(&plain);
                b.truncate(b.len() - 1);
                b
            },
        ] {
            if let Err(e) = verify_attachment(blob.as_slice(), blob.len() as u64, &frozen, &key()).await {
                messages.insert(e.to_string());
            }
        }
        assert_eq!(messages.len(), 1, "拒绝理由不该泄露到底哪一项不匹配: {messages:?}");
    }

    /// 🔴 真实的流不会一次交出全部字节。
    ///
    /// 用切片测只证明"在数据一次到位时它能算对"。S3 回读是一段段来的，
    /// `read_exact` 必须自己把块拼齐——少了这条，一个只在真机上出现的截断 bug
    /// 可以带着"全绿"的单测上线。
    #[tokio::test]
    async fn a_reader_that_dribbles_bytes_still_verifies() {
        struct Dribble {
            data: Vec<u8>,
            pos: usize,
        }
        impl AsyncRead for Dribble {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                // 每次最多 7 字节，故意不对齐任何边界。
                let n = buf.remaining().min(7).min(self.data.len() - self.pos);
                let pos = self.pos;
                buf.put_slice(&self.data[pos..pos + n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(()))
            }
        }

        let plain = vec![9u8; (2 * CHUNK + 33) as usize];
        let blob = sealed(&plain);
        let expected = hex::encode(Sha256::digest(&blob));
        let v = verify_attachment(
            Dribble { data: blob.clone(), pos: 0 },
            blob.len() as u64,
            &frozen_for(&plain, CHUNK),
            &key(),
        )
        .await
        .expect("分段到达的流必须照样验得过");
        assert_eq!(v.sealed_sha256, expected);
    }

    /// 读到一半出错必须整体失败，不能返回"部分成功"。
    ///
    /// S3 回读中断是常态（连接断、超时）。把半份对象当成验过的，等于把一份不完整的
    /// 内容发布进秒传索引。
    #[tokio::test]
    async fn a_read_error_midway_fails_the_whole_verification() {
        struct FailsAfter {
            data: Vec<u8>,
            pos: usize,
            fail_at: usize,
        }
        impl AsyncRead for FailsAfter {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if self.pos >= self.fail_at {
                    return std::task::Poll::Ready(Err(std::io::Error::other("connection reset")));
                }
                let n = buf.remaining().min(64).min(self.fail_at - self.pos);
                let pos = self.pos;
                buf.put_slice(&self.data[pos..pos + n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(()))
            }
        }

        let plain = vec![3u8; (2 * CHUNK) as usize];
        let blob = sealed(&plain);
        let fail_at = blob.len() / 2;
        let err = verify_attachment(
            // 🔴 声称长度正确，但流中途断开——这正是必须判成可重试的场景。
            FailsAfter { data: blob.clone(), pos: 0, fail_at },
            blob.len() as u64,
            &frozen_for(&plain, CHUNK),
            &key(),
        )
        .await
        .expect_err("读取中断必须整体失败");
        // 🔴 而且必须是**可重试**的失败，不是客户端内容错误。
        //
        // 判成 Validation 就是给客户端一个终局 400：一次 COS 抖动让这次上传永久失败，
        // 而字节其实好好地躺在桶里。
        assert!(
            matches!(err, ServerError::ServiceUnavailable(_)),
            "存储读取失败必须是可重试的 5xx，得到: {err:?}"
        );
    }

    /// 错误 magic / 错误版本 / 认证失败：对外必须是同一句话。
    ///
    /// 上一版把 protocol 的错误直接透传，这三种会说出三句不同的话，而当时的测试
    /// 只覆盖了碰巧相同的那几种失败，没发现。
    #[tokio::test]
    async fn malformed_headers_are_indistinguishable_from_other_failures() {
        let plain = vec![5u8; 100];
        let frozen = frozen_for(&plain, CHUNK);
        let good = sealed(&plain);

        let mut not_a_blob = good.clone();
        not_a_blob[0] = b'X';
        let mut bad_version = good.clone();
        bad_version[2] = 0xfe;
        let mut tampered = good.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0xff;

        for blob in [not_a_blob, bad_version, tampered] {
            let err = verify_attachment(blob.as_slice(), blob.len() as u64, &frozen, &key())
                .await
                .expect_err("必须拒绝");
            assert!(err.to_string().ends_with(REJECTION), "拒绝理由不该区分失败原因: {err}");
        }
    }

    /// 🔴 对象不完整由**权威长度**判定，而且发生在读第一个字节之前。
    ///
    /// 这是唯一能把"对象本身就短"和"读到一半断线"分开的地方——`AsyncRead` 分不清，
    /// 两者都表现为 EOF。前者是客户端的事（终局拒绝），后者是存储抖动（可重试）；
    /// 靠 EOF 去猜的话，必然二选一地弄错一种。
    #[tokio::test]
    async fn a_short_object_is_rejected_before_any_byte_is_read() {
        let plain = vec![4u8; (2 * CHUNK) as usize];
        let frozen = frozen_for(&plain, CHUNK);
        let mut blob = sealed(&plain);
        blob.truncate(blob.len() - 20);

        // 权威长度就是它实际的长度：比冻结的短，读之前就该拒。
        let err = verify_attachment(blob.as_slice(), blob.len() as u64, &frozen, &key())
            .await
            .expect_err("长度不符必须失败");
        assert!(
            matches!(err, ServerError::Validation(_)),
            "对象不完整是内容问题，重试多少次都一样: {err:?}"
        );
        assert!(err.to_string().ends_with(REJECTION), "{err}");
    }

    /// 声称的长度对、但流提前 EOF：必须是可重试，不是终局拒绝。
    ///
    /// HEAD 说对象是完整的，回读却提前结束——这只可能是传输问题。判成 400 就是让一次
    /// 抖动把一份好好躺在桶里的对象永久废掉。
    #[tokio::test]
    async fn a_premature_eof_on_a_correctly_sized_object_is_retryable() {
        struct StopsEarly {
            data: Vec<u8>,
            pos: usize,
            stop_at: usize,
        }
        impl AsyncRead for StopsEarly {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                // 到了 stop_at 就装作正常结束（返回 0 字节 = EOF）。
                let n = buf.remaining().min(64).min(self.stop_at.saturating_sub(self.pos));
                let pos = self.pos;
                buf.put_slice(&self.data[pos..pos + n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(()))
            }
        }

        let plain = vec![6u8; (2 * CHUNK) as usize];
        let blob = sealed(&plain);
        let stop_at = blob.len() / 2;
        let err = verify_attachment(
            StopsEarly { data: blob.clone(), pos: 0, stop_at },
            // 权威长度说它是完整的。
            blob.len() as u64,
            &frozen_for(&plain, CHUNK),
            &key(),
        )
        .await
        .expect_err("提前 EOF 必须失败");
        assert!(
            matches!(err, ServerError::ServiceUnavailable(_)),
            "长度已核过，提前 EOF 只可能是传输问题: {err:?}"
        );
    }
}
