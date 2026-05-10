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

//! Wire-adjacent JSON DTOs for the server↔application boundary
//! (spec `02-server/CHANNEL_TRANSFER_SPEC` §5.1).

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use serde::{Deserialize, Serialize};

/// HTTP header carrying the master key on internal Channel Transfer calls
/// (spec §13). Uppercase canonical form; matches PrivChat's existing
/// `/api/service/*` `verify_service_key` middleware.
pub const SERVICE_KEY_HEADER: &str = "X-Service-Key";

/// Server-side payload for `POST /service/privchat/transfer/dispatch`
/// (spec §5.1).
///
/// `body` and `data` cross HTTP as base64; this struct uses `Vec<u8>` and
/// serializes via the `base64_bytes` adapter so call-sites stay
/// transport-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardTransferRequest {
    pub internal_request_id: String,
    pub client_request_id: String,
    pub channel_id: u64,
    pub room_id: u64,
    pub user_id: u64,
    pub route: String,
    #[serde(with = "base64_bytes")]
    pub body: Vec<u8>,
    pub trace_id: String,
}

/// Server-side payload of the application's response on the same endpoint
/// (spec §5.1, flat shape — *not* `ApiEnvelope`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardTransferResponse {
    pub client_request_id: String,
    pub channel_id: u64,
    pub code: i32,
    pub message: String,
    #[serde(with = "base64_bytes_opt", default)]
    pub data: Option<Vec<u8>>,
}

mod base64_bytes {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&BASE64_STD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        use serde::Deserialize as _;
        let s = String::deserialize(de)?;
        if s.is_empty() {
            return Ok(Vec::new());
        }
        BASE64_STD.decode(&s).map_err(serde::de::Error::custom)
    }
}

mod base64_bytes_opt {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(opt: &Option<Vec<u8>>, ser: S) -> Result<S::Ok, S::Error> {
        match opt {
            Some(bytes) => ser.serialize_str(&BASE64_STD.encode(bytes)),
            None => ser.serialize_str(""),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Vec<u8>>, D::Error> {
        use serde::Deserialize as _;
        // Tolerate missing field, JSON null, and empty string — all decode to None.
        let opt: Option<String> = Option::deserialize(de)?;
        let s = match opt {
            None => return Ok(None),
            Some(s) if s.is_empty() => return Ok(None),
            Some(s) => s,
        };
        BASE64_STD
            .decode(&s)
            .map(Some)
            .map_err(serde::de::Error::custom)
    }
}
