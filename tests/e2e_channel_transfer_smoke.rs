// Channel Transfer end-to-end smoke test.
//
// This test spawns the **real** module-privchat smoke binary
// (neton-application-module-privchat/smoke) which boots a CIO ktor server
// hosting the actual PrivChatTransferController + PrivChatTransferDispatcherImpl
// + PrivChatTransferServiceRegistry + BotEchoTransferHandler. We then drive
// it from the server side using:
//
//   - the real ChannelTransferRelayClient for the application boundary path
//     (validates the JSON wire format end-to-end)
//   - the real process_transfer_request() for full wire-in / wire-out
//     coverage (validates server's TransferRequest decode → app dispatch →
//     TransferResponse encode round-trip)
//
// Marked #[ignore] so `cargo test` doesn't accidentally try to run it
// without the smoke binary built. The wrapper script
// `scripts/e2e-channel-transfer-smoke.sh` handles building, env vars,
// and report aggregation.
//
// Required env var:
//   SMOKE_BINARY  — absolute path to the built Kotlin/Native kexe
//
// Output (relative to crate dir):
//   target/e2e/channel-transfer-smoke-report.json
//   target/e2e/channel-transfer-smoke-report.md
//   target/e2e/channel-transfer-smoke.log

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use privchat::channel_transfer::{
    ChannelTransferRelayClient, ForwardTransferRequest,
};
use privchat::config::ChannelTransferConfig;
use privchat::handler::channel_transfer_handler::{
    process_transfer_request, ChannelTransferLookups, CODE_CHANNEL_NOT_SUBSCRIBED,
};
use serde_json::json;

const SHARED_KEY: &str = "smoke-shared-master-key-0123456789";
const CHANNEL_ID: u64 = 2817;
const ROOM_ID: u64 = 2817;
const USER_ID: u64 = 100002077;
const SMOKE_PORT: u16 = 19200;
const READY_TIMEOUT_SECS: u64 = 15;

// =====================================================================
// Smoke binary lifecycle
// =====================================================================

struct Smoke {
    child: Child,
    port: u16,
}

impl Drop for Smoke {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_smoke() -> Smoke {
    let binary = std::env::var("SMOKE_BINARY")
        .expect("SMOKE_BINARY env var not set; use scripts/e2e-channel-transfer-smoke.sh");
    let mut child = Command::new(&binary)
        .arg(format!("--port={}", SMOKE_PORT))
        .arg(format!("--master-key={}", SHARED_KEY))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn smoke binary {binary}: {e}"));

    let stdout = child.stdout.take().expect("child stdout");
    let reader = BufReader::new(stdout);
    let deadline = Instant::now() + Duration::from_secs(READY_TIMEOUT_SECS);
    let mut bound_port: Option<u16> = None;
    for line in reader.lines() {
        let line = line.expect("read stdout line");
        eprintln!("[smoke stdout] {line}");
        if let Some(rest) = line.strip_prefix("READY port=") {
            bound_port = Some(rest.trim().parse().unwrap_or_else(|e| {
                panic!("READY line did not parse a port: '{line}': {e}")
            }));
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("smoke binary did not announce READY within {READY_TIMEOUT_SECS}s");
        }
    }
    let port = bound_port.expect("READY line not seen on stdout before EOF");
    Smoke { child, port }
}

// =====================================================================
// Lookups stub for process_transfer_request (server-side gate testing)
// =====================================================================

struct CannedLookups {
    is_subscribed: bool,
}

#[async_trait]
impl ChannelTransferLookups for CannedLookups {
    async fn is_subscribed(&self, _user_id: u64, _channel_id: u64) -> bool {
        self.is_subscribed
    }
    async fn resolve_room(&self, _channel_id: u64) -> Option<u64> {
        Some(ROOM_ID)
    }
    async fn is_room_member(&self, _user_id: u64, _room_id: u64) -> bool {
        true
    }
}

// =====================================================================
// Test
// =====================================================================

#[tokio::test]
#[ignore]
async fn channel_transfer_e2e_smoke() {
    let started_at = Instant::now();
    let smoke = spawn_smoke();
    let app_url = format!("http://127.0.0.1:{}", smoke.port);

    let cfg = ChannelTransferConfig {
        application_url: app_url.clone(),
        application_master_key: SHARED_KEY.to_string(),
        timeout_ms: 5_000,
    };
    let relay = ChannelTransferRelayClient::new(&cfg).expect("build relay client");
    let relay_arc = Arc::new(relay);

    let mut cases: Vec<serde_json::Value> = Vec::new();

    // --- Phase A: direct relay (app boundary contract) ---

    // A1: success path → bot/echo/ping
    {
        let req = ForwardTransferRequest {
            internal_request_id: "srv_req_a1".to_string(),
            client_request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            channel_id: CHANNEL_ID,
            room_id: ROOM_ID,
            user_id: USER_ID,
            route: "bot/echo/ping".to_string(),
            body: Vec::new(),
            trace_id: "trace_a1".to_string(),
        };
        let t0 = Instant::now();
        let resp = relay_arc.forward(&req).await.expect("relay success");
        let elapsed = t0.elapsed();
        let data_len = resp.data.as_ref().map(|d| d.len()).unwrap_or(0);
        let data_digest = resp
            .data
            .as_ref()
            .map(|d| String::from_utf8_lossy(d).to_string())
            .unwrap_or_default();
        assert_eq!(resp.code, 0, "expected code=0 for bot/echo/ping");
        assert_eq!(resp.message, "OK");
        assert_eq!(data_digest, "pong", "expected handler to echo 'pong'");
        cases.push(json!({
            "phase": "A",
            "name": "relay_bot_echo_ping_success",
            "request_id": req.client_request_id,
            "route": req.route,
            "response_code": resp.code,
            "response_message": resp.message,
            "response_data_len": data_len,
            "response_data_digest": data_digest,
            "elapsed_ms": elapsed.as_millis(),
        }));
    }

    // A2: wrong route prefix → app returns 20902
    {
        let req = ForwardTransferRequest {
            internal_request_id: "srv_req_a2".to_string(),
            client_request_id: "650e8400-e29b-41d4-a716-446655440002".to_string(),
            channel_id: CHANNEL_ID,
            room_id: ROOM_ID,
            user_id: USER_ID,
            route: "wallet/balance/query".to_string(),
            body: Vec::new(),
            trace_id: "trace_a2".to_string(),
        };
        let t0 = Instant::now();
        let resp = relay_arc.forward(&req).await.expect("relay success (HTTP 200)");
        let elapsed = t0.elapsed();
        assert_eq!(resp.code, 20902, "expected TransferServiceNotFound on prefix mismatch");
        assert!(
            resp.message.contains("wallet") && resp.message.contains("bot"),
            "expected message to mention both prefixes; got: {}",
            resp.message
        );
        cases.push(json!({
            "phase": "A",
            "name": "relay_wrong_route_prefix",
            "request_id": req.client_request_id,
            "route": req.route,
            "response_code": resp.code,
            "response_message": resp.message,
            "elapsed_ms": elapsed.as_millis(),
        }));
    }

    // A3: idempotency replay (same client_request_id as A1 → app should replay)
    {
        let req = ForwardTransferRequest {
            internal_request_id: "srv_req_a3_replay".to_string(),
            client_request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            channel_id: CHANNEL_ID,
            room_id: ROOM_ID,
            user_id: USER_ID,
            route: "bot/echo/ping".to_string(),
            body: b"different body".to_vec(),
            trace_id: "trace_a3".to_string(),
        };
        let t0 = Instant::now();
        let resp = relay_arc.forward(&req).await.expect("relay success");
        let elapsed = t0.elapsed();
        let data_digest = resp
            .data
            .as_ref()
            .map(|d| String::from_utf8_lossy(d).to_string())
            .unwrap_or_default();
        assert_eq!(resp.code, 0);
        assert_eq!(data_digest, "pong", "replay should return same data as first call");
        cases.push(json!({
            "phase": "A",
            "name": "relay_idempotency_replay",
            "request_id": req.client_request_id,
            "route": req.route,
            "response_code": resp.code,
            "response_data_digest": data_digest,
            "elapsed_ms": elapsed.as_millis(),
        }));
    }

    // --- Phase B: server-side wire path with subscribed=false ---
    // Validates server's ChannelTransferHandler short-circuits at the
    // subscription gate without reaching application.

    {
        let wire_req = privchat_protocol::TransferRequest {
            request_id: "850e8400-e29b-41d4-a716-446655440008".to_string(),
            channel_id: CHANNEL_ID,
            route: "bot/echo/ping".to_string(),
            body: Vec::new(),
        };
        let raw = privchat_protocol::encode_message(&wire_req).expect("encode wire");
        let lookups = CannedLookups { is_subscribed: false };
        let t0 = Instant::now();
        let bytes = process_transfer_request(&raw, Some(USER_ID), &lookups, &relay_arc)
            .await
            .expect("encode response");
        let elapsed = t0.elapsed();
        let resp: privchat_protocol::TransferResponse =
            privchat_protocol::decode_message(&bytes).expect("decode wire response");
        assert_eq!(resp.code, CODE_CHANNEL_NOT_SUBSCRIBED, "expected 20900 on unsubscribed");
        assert_eq!(resp.request_id, wire_req.request_id);
        assert_eq!(resp.channel_id, wire_req.channel_id);
        cases.push(json!({
            "phase": "B",
            "name": "wire_unsubscribed_short_circuits",
            "request_id": wire_req.request_id,
            "route": wire_req.route,
            "response_code": resp.code,
            "response_message": resp.message,
            "elapsed_ms": elapsed.as_millis(),
        }));
    }

    // --- Phase C: full wire-to-wire chain with subscribed=true ---
    // Wire decode → relay → smoke server → relay response → wire encode.

    {
        let wire_req = privchat_protocol::TransferRequest {
            request_id: "950e8400-e29b-41d4-a716-446655440009".to_string(),
            channel_id: CHANNEL_ID,
            route: "bot/echo/ping".to_string(),
            body: b"wire-payload".to_vec(),
        };
        let raw = privchat_protocol::encode_message(&wire_req).expect("encode wire");
        let lookups = CannedLookups { is_subscribed: true };
        let t0 = Instant::now();
        let bytes = process_transfer_request(&raw, Some(USER_ID), &lookups, &relay_arc)
            .await
            .expect("encode response");
        let elapsed = t0.elapsed();
        let resp: privchat_protocol::TransferResponse =
            privchat_protocol::decode_message(&bytes).expect("decode wire response");
        assert_eq!(resp.code, 0, "expected wire code=0 for full chain success");
        assert_eq!(resp.message, "OK");
        assert_eq!(resp.request_id, wire_req.request_id, "client_request_id preserved end-to-end");
        assert_eq!(resp.channel_id, wire_req.channel_id);
        let data = resp.data.as_deref().unwrap_or(&[]);
        assert_eq!(
            std::str::from_utf8(data).unwrap_or(""),
            "pong",
            "wire response data should round-trip handler output",
        );
        cases.push(json!({
            "phase": "C",
            "name": "wire_full_chain_success",
            "request_id": wire_req.request_id,
            "route": wire_req.route,
            "response_code": resp.code,
            "response_message": resp.message,
            "response_data_digest": std::str::from_utf8(data).unwrap_or(""),
            "elapsed_ms": elapsed.as_millis(),
        }));
    }

    // --- Report generation ---

    let total_elapsed = started_at.elapsed();
    let report = json!({
        "spec_version": "v1.0",
        "started_at_unix_ms": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        "total_elapsed_ms": total_elapsed.as_millis(),
        "protocol_commit": std::env::var("PROTOCOL_COMMIT").unwrap_or_default(),
        "server_commit": std::env::var("SERVER_COMMIT").unwrap_or_default(),
        "application_commit": std::env::var("APPLICATION_COMMIT").unwrap_or_default(),
        "smoke_binary": std::env::var("SMOKE_BINARY").unwrap_or_default(),
        "smoke_port": smoke.port,
        "shared_master_key_len": SHARED_KEY.len(),
        "channel_id": CHANNEL_ID,
        "room_id": ROOM_ID,
        "user_id": USER_ID,
        "phase_a": "direct relay client → app HTTP boundary",
        "phase_b": "server wire ingress with subscribed=false (subscription gate)",
        "phase_c": "full wire-to-wire chain with subscribed=true",
        "cases": cases,
    });

    let target_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/e2e");
    std::fs::create_dir_all(&target_dir).expect("create target/e2e");

    let json_path = target_dir.join("channel-transfer-smoke-report.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&report).unwrap())
        .expect("write json report");

    let md = format_md(&report);
    let md_path = target_dir.join("channel-transfer-smoke-report.md");
    std::fs::write(&md_path, md).expect("write md report");

    eprintln!("[smoke] report: {}", json_path.display());
    eprintln!("[smoke] report: {}", md_path.display());
}

fn format_md(report: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str("# Channel Transfer E2E Smoke Report\n\n");
    out.push_str(&format!(
        "- spec: {}\n- total elapsed: {}ms\n- smoke port: {}\n- channel_id: {} / user_id: {}\n",
        report["spec_version"].as_str().unwrap_or(""),
        report["total_elapsed_ms"],
        report["smoke_port"],
        report["channel_id"],
        report["user_id"],
    ));
    out.push_str(&format!(
        "- protocol_commit: `{}`\n- server_commit: `{}`\n- application_commit: `{}`\n\n",
        report["protocol_commit"].as_str().unwrap_or("(unset)"),
        report["server_commit"].as_str().unwrap_or("(unset)"),
        report["application_commit"].as_str().unwrap_or("(unset)"),
    ));
    out.push_str("## Phases\n\n");
    out.push_str(&format!("- **A**: {}\n", report["phase_a"].as_str().unwrap_or("")));
    out.push_str(&format!("- **B**: {}\n", report["phase_b"].as_str().unwrap_or("")));
    out.push_str(&format!("- **C**: {}\n\n", report["phase_c"].as_str().unwrap_or("")));
    out.push_str("## Cases\n\n");
    out.push_str("| phase | name | route | code | elapsed_ms |\n");
    out.push_str("|-------|------|-------|------|-----------|\n");
    if let Some(cases) = report["cases"].as_array() {
        for c in cases {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                c["phase"].as_str().unwrap_or(""),
                c["name"].as_str().unwrap_or(""),
                c["route"].as_str().unwrap_or(""),
                c["response_code"],
                c["elapsed_ms"],
            ));
        }
    }
    out.push_str("\nAll asserts passed.\n");
    out
}
