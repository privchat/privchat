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

//! The single device-id contract shared by every client and the server.
//!
//! A device id is a random UUID version 4 (RFC 4122 variant) written in its
//! canonical form: lowercase, hyphenated 8-4-4-4-12, e.g.
//! `550e8400-e29b-41d4-a716-446655440000`.
//!
//! Anything else is rejected outright: uppercase, other versions (v1/v3/v5/...),
//! the nil UUID, simple/braced/urn forms, or a non-RFC 4122 variant. There is no
//! normalisation — a value that is accepted is stored and echoed byte-for-byte,
//! so text columns and `uuid::text` comparisons always agree.

use crate::error::{Result, ServerError};
use uuid::{Uuid, Variant};

/// Length of the canonical hyphenated form.
const CANONICAL_LEN: usize = 36;

/// Returns the parsed UUID when `input` is a canonical device id, `None` otherwise.
fn parse_canonical(input: &str) -> Option<Uuid> {
    // Cheap reject first: only the hyphenated form has exactly 36 chars, which
    // also rules out simple (32), braced (38) and urn (45) forms.
    if input.len() != CANONICAL_LEN {
        return None;
    }
    let uuid = Uuid::try_parse(input).ok()?;
    if uuid.get_version_num() != 4 || uuid.get_variant() != Variant::RFC4122 {
        return None;
    }
    // `hyphenated()` always renders lowercase; equality rejects any uppercase digit.
    let mut buf = Uuid::encode_buffer();
    if uuid.hyphenated().encode_lower(&mut buf) != input {
        return None;
    }
    Some(uuid)
}

/// Whether `input` satisfies the canonical device-id contract.
pub fn is_canonical_device_id(input: &str) -> bool {
    parse_canonical(input).is_some()
}

/// Validates a client-supplied device id. Fails with [`ServerError::InvalidDeviceId`].
pub fn validate_device_id(input: &str) -> Result<()> {
    parse_device_id(input).map(|_| ())
}

/// Validates a device id and returns it as a [`Uuid`] for `uuid` columns.
/// Fails with [`ServerError::InvalidDeviceId`].
pub fn parse_device_id(input: &str) -> Result<Uuid> {
    parse_canonical(input).ok_or(ServerError::InvalidDeviceId)
}

/// Generates a new server-side device id (always canonical).
pub fn generate_device_id() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const V4: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn accepts_canonical_v4() {
        assert!(is_canonical_device_id(V4));
        assert_eq!(parse_device_id(V4).unwrap().to_string(), V4);
        // every RFC 4122 variant nibble
        for variant in ['8', '9', 'a', 'b'] {
            let id = format!("550e8400-e29b-41d4-{variant}716-446655440000");
            assert!(is_canonical_device_id(&id), "{id}");
        }
    }

    #[test]
    fn accepts_generated_ids() {
        for _ in 0..256 {
            assert!(is_canonical_device_id(&generate_device_id()));
        }
    }

    #[test]
    fn rejects_uppercase_v4() {
        assert!(!is_canonical_device_id(&V4.to_uppercase()));
        assert!(!is_canonical_device_id(
            "550E8400-e29b-41d4-a716-446655440000"
        ));
    }

    #[test]
    fn rejects_other_versions() {
        // v1
        assert!(!is_canonical_device_id(
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
        ));
        // v3
        assert!(!is_canonical_device_id(
            "6fa459ea-ee8a-3ca4-894e-db77e160355e"
        ));
        // v5
        assert!(!is_canonical_device_id(
            "886313e1-3b8a-5372-9b90-0c9aee199e5d"
        ));
        // v7
        assert!(!is_canonical_device_id(
            "017f22e2-79b0-7cc3-98c4-dc0c0c07398f"
        ));
        // fixture-style pattern that is not a v4
        assert!(!is_canonical_device_id(
            "11111111-2222-3333-4444-555555555555"
        ));
    }

    #[test]
    fn rejects_nil_and_max() {
        assert!(!is_canonical_device_id(
            "00000000-0000-0000-0000-000000000000"
        ));
        assert!(!is_canonical_device_id(
            "ffffffff-ffff-ffff-ffff-ffffffffffff"
        ));
    }

    #[test]
    fn rejects_non_hyphenated_forms() {
        assert!(!is_canonical_device_id("550e8400e29b41d4a716446655440000"));
        assert!(!is_canonical_device_id(
            "{550e8400-e29b-41d4-a716-446655440000}"
        ));
        assert!(!is_canonical_device_id(
            "urn:uuid:550e8400-e29b-41d4-a716-446655440000"
        ));
        // misplaced hyphens, same length
        assert!(!is_canonical_device_id(
            "550e8400e-29b-41d4-a716-446655440000"
        ));
    }

    #[test]
    fn rejects_wrong_variant() {
        // NCS (0xxx), Microsoft (110x), future (111x)
        for variant in ['0', '7', 'c', 'd', 'e', 'f'] {
            let id = format!("550e8400-e29b-41d4-{variant}716-446655440000");
            assert!(!is_canonical_device_id(&id), "{id}");
        }
    }

    #[test]
    fn rejects_empty_whitespace_and_garbage() {
        assert!(!is_canonical_device_id(""));
        assert!(!is_canonical_device_id(" "));
        assert!(!is_canonical_device_id(&format!(" {V4}")));
        assert!(!is_canonical_device_id(&format!("{V4} ")));
        assert!(!is_canonical_device_id("dev-abc"));
        assert!(!is_canonical_device_id(
            "550e8400-e29b-41d4-a716-44665544000g"
        ));
    }

    #[test]
    fn errors_are_invalid_device_id() {
        assert!(matches!(
            validate_device_id("dev-abc"),
            Err(ServerError::InvalidDeviceId)
        ));
        assert!(matches!(
            parse_device_id(""),
            Err(ServerError::InvalidDeviceId)
        ));
    }
}
