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

//! `message/history/search` — 用户级云端历史搜索（MESSAGE_HISTORY_AND_SEARCH spec §4）。
//!
//! 与 admin 的 `/api/service/messages/search`（全库、X-Service-Key、offset 分页）
//! 完全独立：这里的一切都以"当前用户可见范围"为边界。
//!
//! V1 契约：
//! - scope GLOBAL（EXISTS semi-join participants）/ CHANNEL（先过 ensure_channel_visible）
//! - revoked=false AND deleted=false（撤回正文库内未清除，必须显式过滤防 snippet 泄露）
//! - keyset：created_at DESC, message_id DESC；cursor = "created_at:message_id"
//! - query 字符数 [2, 64]；limit 上限 50；per-user 限频；statement_timeout + tokio 双超时
//! - 命中返回 snippet 投影 + highlight_ranges（字符偏移），**不回填本地 message 表**
//! - sender_user_id / message_types / time_range 为预留字段，V1 忽略

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// per-user 最小搜索间隔（简单限频，防脚本刷全库扫描；
/// 参照 TypingRateLimiter 的 per-key 间隔思路，规模小到不值得独立结构）。
const MIN_SEARCH_INTERVAL: Duration = Duration::from_millis(300);
/// tokio 侧整体超时（statement_timeout=4s 的外层兜底）。
const SEARCH_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_QUERY_CHARS: usize = 64;
const MIN_QUERY_CHARS: usize = 2;
const MAX_LIMIT: u64 = 50;
const DEFAULT_LIMIT: u64 = 20;
/// snippet 命中词两侧各保留的字符数
const SNIPPET_CONTEXT_CHARS: usize = 24;

fn rate_limiter() -> &'static Mutex<HashMap<u64, Instant>> {
    static LIMITER: OnceLock<Mutex<HashMap<u64, Instant>>> = OnceLock::new();
    LIMITER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn check_rate_limit(user_id: u64) -> RpcResult<()> {
    let mut map = rate_limiter().lock().expect("search rate limiter poisoned");
    let now = Instant::now();
    if let Some(last) = map.get(&user_id) {
        if now.duration_since(*last) < MIN_SEARCH_INTERVAL {
            return Err(RpcError::from_code(
                privchat_protocol::ErrorCode::RateLimitExceeded,
                "search rate limit exceeded".to_string(),
            ));
        }
    }
    map.insert(user_id, now);
    // 防止 map 无界增长（活跃搜索用户量级很小，粗暴清理即可）
    if map.len() > 10_000 {
        map.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
    }
    Ok(())
}

/// ILIKE 模式转义：\ % _ 是 LIKE 元字符，query 里出现时按字面匹配。
fn escape_ilike(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len() + 8);
    for ch in query.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    format!("%{}%", escaped)
}

/// 大小写不敏感的字符级 snippet + 命中区间（相对 snippet 的字符偏移，[start,end)）。
/// 按字符不按字节：客户端（Kotlin/TS/Swift）都以字符/码点处理高亮更稳。
fn build_snippet(content: &str, query: &str) -> (String, Vec<(u32, u32)>) {
    let lower1 = |c: char| c.to_lowercase().next().unwrap_or(c);
    let chars: Vec<char> = content.chars().collect();
    let lc: Vec<char> = chars.iter().map(|c| lower1(*c)).collect();
    let q: Vec<char> = query.chars().map(lower1).collect();
    let qlen = q.len();

    // 找第一处命中
    let mut first: Option<usize> = None;
    if qlen > 0 && lc.len() >= qlen {
        'outer: for i in 0..=(lc.len() - qlen) {
            for j in 0..qlen {
                if lc[i + j] != q[j] {
                    continue 'outer;
                }
            }
            first = Some(i);
            break;
        }
    }

    let (start, end) = match first {
        Some(i) => (
            i.saturating_sub(SNIPPET_CONTEXT_CHARS),
            (i + qlen + SNIPPET_CONTEXT_CHARS).min(chars.len()),
        ),
        // 理论上 ILIKE recheck 保证有命中；兜底取头部
        None => (0, chars.len().min(60)),
    };

    let prefix_ellipsis = start > 0;
    let suffix_ellipsis = end < chars.len();
    let offset = if prefix_ellipsis { 1u32 } else { 0 };

    let mut snippet = String::new();
    if prefix_ellipsis {
        snippet.push('…');
    }
    snippet.extend(&chars[start..end]);
    if suffix_ellipsis {
        snippet.push('…');
    }

    // 窗口内全部命中（相对 snippet 偏移），上限 8 段
    let mut ranges = Vec::new();
    if qlen > 0 && end - start >= qlen {
        let win = &lc[start..end];
        let mut i = 0;
        while i + qlen <= win.len() && ranges.len() < 8 {
            if win[i..i + qlen] == q[..] {
                ranges.push((i as u32 + offset, (i + qlen) as u32 + offset));
                i += qlen;
            } else {
                i += 1;
            }
        }
    }
    (snippet, ranges)
}

fn parse_cursor(raw: &str) -> RpcResult<(i64, i64)> {
    let (ts, id) = raw
        .split_once(':')
        .ok_or_else(|| RpcError::validation("invalid cursor".to_string()))?;
    let ts = ts
        .parse::<i64>()
        .map_err(|_| RpcError::validation("invalid cursor".to_string()))?;
    let id = id
        .parse::<i64>()
        .map_err(|_| RpcError::validation("invalid cursor".to_string()))?;
    if ts <= 0 || id <= 0 {
        return Err(RpcError::validation("invalid cursor".to_string()));
    }
    Ok((ts, id))
}

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    check_rate_limit(user_id)?;

    // ---- 参数 ----
    let query = body
        .get("query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    let query_chars = query.chars().count();
    if query_chars < MIN_QUERY_CHARS {
        return Err(RpcError::validation(format!(
            "query requires at least {} characters",
            MIN_QUERY_CHARS
        )));
    }
    if query_chars > MAX_QUERY_CHARS {
        return Err(RpcError::validation(format!(
            "query exceeds {} characters",
            MAX_QUERY_CHARS
        )));
    }

    let scope = body
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("GLOBAL");
    let scope_channel = match scope {
        "GLOBAL" => None,
        "CHANNEL" => {
            let channel_id = body
                .get("channel_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| {
                    RpcError::validation("channel_id is required for CHANNEL scope".to_string())
                })?;
            // CHANNEL scope：显式可见性守卫（非成员/不存在统一 not_found，spec §3）
            crate::rpc::ensure_channel_visible(
                services.channel_service.as_ref(),
                channel_id,
                user_id,
            )
            .await?;
            Some(channel_id)
        }
        other => {
            return Err(RpcError::validation(format!(
                "invalid scope: {} (expected GLOBAL or CHANNEL)",
                other
            )))
        }
    };

    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT) as i64;

    let cursor = body
        .get("cursor")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(parse_cursor)
        .transpose()?;

    // ---- 分词（单一真源：库内 privchat_search_tokens，与 011 索引共用）----
    let repo = services.message_repository.as_ref();
    let tsquery = repo
        .search_tokens_tsquery(query)
        .await
        .map_err(|e| RpcError::internal(format!("search tokenize failed: {}", e)))?;
    if tsquery.is_empty() {
        // query 全是分不出 token 的字符（如纯标点）：语义上无可命中
        return Ok(json!({ "hits": [], "next_cursor": null }));
    }
    let ilike_pattern = escape_ilike(query);

    // ---- 查询（statement_timeout=4s + tokio 6s 双兜底）----
    let hits = tokio::time::timeout(
        SEARCH_TIMEOUT,
        repo.search_visible(
            user_id,
            scope_channel,
            &tsquery,
            &ilike_pattern,
            cursor,
            limit,
        ),
    )
    .await
    .map_err(|_| {
        RpcError::from_code(
            privchat_protocol::ErrorCode::SystemBusy,
            "search timed out".to_string(),
        )
    })?
    .map_err(|e| RpcError::internal(format!("search failed: {}", e)))?;

    let next_cursor = if hits.len() as i64 == limit {
        hits.last()
            .map(|h| format!("{}:{}", h.created_at, h.message_id))
    } else {
        None
    };

    let hit_views: Vec<Value> = hits
        .iter()
        .map(|h| {
            let (snippet, ranges) = build_snippet(&h.content, query);
            let highlight_ranges: Vec<Value> =
                ranges.iter().map(|(s, e)| json!([s, e])).collect();
            json!({
                "channel_id": h.channel_id,
                "message_id": h.message_id,
                "sender_user_id": h.sender_id,
                "created_at": h.created_at,
                "message_type": privchat_protocol::ContentMessageType::from_u32(h.message_type as u32)
                    .unwrap_or(privchat_protocol::ContentMessageType::Text)
                    .as_str(),
                "snippet": snippet,
                "highlight_ranges": highlight_ranges,
            })
        })
        .collect();

    tracing::debug!(
        "🔍 message/history/search: user={}, scope={}, query_chars={}, hits={}, has_next={}",
        user_id,
        scope,
        query_chars,
        hit_views.len(),
        next_cursor.is_some()
    );

    Ok(json!({
        "hits": hit_views,
        "next_cursor": next_cursor,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_ilike_escapes_metacharacters() {
        assert_eq!(escape_ilike("50%_off\\x"), "%50\\%\\_off\\\\x%");
        assert_eq!(escape_ilike("福寿"), "%福寿%");
    }

    #[test]
    fn cursor_roundtrip_and_rejects_garbage() {
        assert_eq!(
            parse_cursor("1751900000000:598433").unwrap(),
            (1751900000000, 598433)
        );
        assert!(parse_cursor("abc").is_err());
        assert!(parse_cursor("1:0").is_err());
        assert!(parse_cursor("-1:5").is_err());
    }

    #[test]
    fn snippet_windows_and_highlights() {
        // 命中居中，两侧截断加省略号，偏移含省略号
        let content = "零".repeat(40) + "福寿万家" + &"零".repeat(40);
        let (snippet, ranges) = build_snippet(&content, "福寿");
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
        assert_eq!(ranges.len(), 1);
        let (s, e) = ranges[0];
        let chars: Vec<char> = snippet.chars().collect();
        assert_eq!(
            chars[s as usize..e as usize].iter().collect::<String>(),
            "福寿"
        );

        // 大小写不敏感
        let (snippet, ranges) = build_snippet("Hello WORLD", "world");
        assert_eq!(ranges.len(), 1);
        assert!(snippet.contains("WORLD"));

        // 多处命中
        let (_, ranges) = build_snippet("红包红包红包", "红包");
        assert_eq!(ranges.len(), 3);
    }
}
