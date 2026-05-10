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

//! Pure server-side string/size validators for Channel Transfer wire fields
//! (spec `02-server/CHANNEL_TRANSFER_SPEC` §6 / §7 / §10).
//!
//! Server-side gates only — business prefix/handler resolution is the
//! application's job (dispatch spec §10.1) and intentionally not done here.

// =====================================================================
// Limits — quoted directly from spec §7 / §10
// =====================================================================

/// Maximum allowed `request_id` length (spec §7.1).
pub const MAX_TRANSFER_REQUEST_ID_LEN: usize = 64;

/// Maximum allowed `route` length (spec §6 — server-side string check).
pub const MAX_TRANSFER_ROUTE_LEN: usize = 256;

/// Maximum allowed `body` / `data` length, in raw bytes (spec §10).
pub const MAX_TRANSFER_BODY_BYTES: usize = 64 * 1024;

// =====================================================================
// Errors
// =====================================================================

/// Errors returned by the pure validator helpers. Mapping to
/// `protocol::ErrorCode` numbers happens in the wire / HTTP layers —
/// keeping the validator transport-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelTransferValidationError {
    RequestIdEmpty,
    RequestIdTooLong { len: usize },
    RequestIdLowEntropy,
    RequestIdBadChar,
    RouteEmpty,
    RouteTooLong { len: usize },
    RouteLeadingSlash,
    RouteEmptySegment,
    RouteSegmentBadChar,
    RouteShape,
    BodyTooLarge { len: usize, limit: usize },
}

impl std::fmt::Display for ChannelTransferValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ChannelTransferValidationError::*;
        match self {
            RequestIdEmpty => write!(f, "request_id is empty"),
            RequestIdTooLong { len } => {
                write!(
                    f,
                    "request_id length {len} exceeds {MAX_TRANSFER_REQUEST_ID_LEN}"
                )
            }
            RequestIdLowEntropy => write!(
                f,
                "request_id entropy too low (need UUID v4 or >=16 random bytes)"
            ),
            RequestIdBadChar => write!(f, "request_id contains non-URL-safe character"),
            RouteEmpty => write!(f, "route is empty"),
            RouteTooLong { len } => {
                write!(f, "route length {len} exceeds {MAX_TRANSFER_ROUTE_LEN}")
            }
            RouteLeadingSlash => write!(
                f,
                "route must not start with '/' (format: service/module/action)"
            ),
            RouteEmptySegment => write!(f, "route contains empty segment"),
            RouteSegmentBadChar => write!(
                f,
                "route segment contains non-lowercase / non-kebab-case character"
            ),
            RouteShape => write!(
                f,
                "route must have exactly 3 segments (service/module/action)"
            ),
            BodyTooLarge { len, limit } => {
                write!(f, "body size {len} bytes exceeds {limit}")
            }
        }
    }
}

impl std::error::Error for ChannelTransferValidationError {}

// =====================================================================
// Validators
// =====================================================================

/// Validate `TransferRequest.request_id` per spec §7.1:
///
/// - non-empty
/// - length ≤ 64
/// - URL/log-safe ASCII characters
/// - sufficient entropy: either UUID v4 (with hyphens) or ≥16 alnum bytes
///   (Crockford-base32 / base32hex / base64url-no-pad / hex etc.)
///
/// Servers should reject with `10100 InvalidParams` on failure.
pub fn validate_transfer_request_id(
    request_id: &str,
) -> Result<(), ChannelTransferValidationError> {
    if request_id.is_empty() {
        return Err(ChannelTransferValidationError::RequestIdEmpty);
    }
    if request_id.len() > MAX_TRANSFER_REQUEST_ID_LEN {
        return Err(ChannelTransferValidationError::RequestIdTooLong {
            len: request_id.len(),
        });
    }
    if !request_id
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
    {
        return Err(ChannelTransferValidationError::RequestIdBadChar);
    }

    // UUID v4 form: 8-4-4-4-12 hex with hyphens, length 36, version nibble = '4'
    if request_id.len() == 36 && uuid_v4_shape(request_id) {
        return Ok(());
    }

    // Otherwise: require ≥16 ASCII-alnum bytes (≥80 bits entropy)
    let alnum_count = request_id
        .bytes()
        .filter(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'))
        .count();
    if alnum_count < 16 {
        return Err(ChannelTransferValidationError::RequestIdLowEntropy);
    }
    Ok(())
}

fn uuid_v4_shape(s: &str) -> bool {
    let bytes = s.as_bytes();
    let hyphens_at = [8usize, 13, 18, 23];
    for (i, b) in bytes.iter().enumerate() {
        let is_hy = hyphens_at.contains(&i);
        if is_hy {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    // version nibble is the first char of the third group (index 14)
    bytes.get(14).copied() == Some(b'4')
}

/// Validate `TransferRequest.route` for *server-side string format* only
/// (spec §6). Format is `service/module/action`:
///
/// - non-empty, length ≤ 256
/// - does not start with '/'
/// - exactly three slash-delimited segments, none empty
/// - each segment is `[a-z0-9-]+`, must not start or end with '-'
///
/// Server does **not** validate the prefix against any service registry —
/// that is the application's job (dispatch spec §10.1).
pub fn validate_transfer_route(route: &str) -> Result<(), ChannelTransferValidationError> {
    if route.is_empty() {
        return Err(ChannelTransferValidationError::RouteEmpty);
    }
    if route.len() > MAX_TRANSFER_ROUTE_LEN {
        return Err(ChannelTransferValidationError::RouteTooLong { len: route.len() });
    }
    if route.starts_with('/') {
        return Err(ChannelTransferValidationError::RouteLeadingSlash);
    }
    let segments: Vec<&str> = route.split('/').collect();
    if segments.len() != 3 {
        return Err(ChannelTransferValidationError::RouteShape);
    }
    for seg in &segments {
        if seg.is_empty() {
            return Err(ChannelTransferValidationError::RouteEmptySegment);
        }
        if !is_kebab_lower(seg) {
            return Err(ChannelTransferValidationError::RouteSegmentBadChar);
        }
    }
    Ok(())
}

fn is_kebab_lower(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.first().copied() == Some(b'-') || bytes.last().copied() == Some(b'-') {
        return false;
    }
    bytes
        .iter()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// Validate `TransferRequest.body` / `TransferResponse.data` size (spec §10).
pub fn validate_transfer_body_size(body: &[u8]) -> Result<(), ChannelTransferValidationError> {
    if body.len() > MAX_TRANSFER_BODY_BYTES {
        return Err(ChannelTransferValidationError::BodyTooLarge {
            len: body.len(),
            limit: MAX_TRANSFER_BODY_BYTES,
        });
    }
    Ok(())
}
