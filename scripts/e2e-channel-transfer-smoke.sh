#!/bin/bash
# Channel Transfer end-to-end smoke runner.
#
# Builds the module-privchat smoke binary (Kotlin/Native), then runs the
# Rust integration test that drives it through real HTTP + the real
# server-side wire layer. Reports go to target/e2e/.
#
# Usage:
#   ./scripts/e2e-channel-transfer-smoke.sh
#
# Exit code 0 = all phases passed; non-zero = test failed.
#
# Outputs (relative to privchat-server crate root):
#   target/e2e/channel-transfer-smoke-report.json
#   target/e2e/channel-transfer-smoke-report.md
#   target/e2e/channel-transfer-smoke.log

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE_ROOT="$(cd "$REPO_ROOT/.." && pwd)"
MODULE_PRIVCHAT="$WORKSPACE_ROOT/neton-application-module-privchat"

OS_TARGET="${OS_TARGET:-macosArm64}"
# Only macosArm64 is wired up in :smoke:; extend per platform if needed.
case "$OS_TARGET" in
  macosArm64) GRADLE_TASK=":smoke:linkReleaseExecutableMacosArm64" ;;
  *) echo "OS_TARGET=$OS_TARGET not yet wired in :smoke: (only macosArm64)"; exit 2 ;;
esac
KEXE_REL_PATH="smoke/build/bin/${OS_TARGET}/smokeReleaseExecutable/smoke.kexe"

REPORT_DIR="$REPO_ROOT/target/e2e"
mkdir -p "$REPORT_DIR"
LOG_FILE="$REPORT_DIR/channel-transfer-smoke.log"

echo "[$(date '+%Y-%m-%d %H:%M:%S')] === Channel Transfer E2E Smoke ===" | tee "$LOG_FILE"
echo "[smoke] target: $OS_TARGET" | tee -a "$LOG_FILE"
echo "[smoke] module: $MODULE_PRIVCHAT" | tee -a "$LOG_FILE"

echo "[smoke] building Kotlin smoke binary ..." | tee -a "$LOG_FILE"
(cd "$MODULE_PRIVCHAT" && ./gradlew "$GRADLE_TASK") 2>&1 | tee -a "$LOG_FILE"

SMOKE_BINARY="$MODULE_PRIVCHAT/$KEXE_REL_PATH"
if [ ! -x "$SMOKE_BINARY" ]; then
  echo "[smoke] FAIL: binary not found at $SMOKE_BINARY" | tee -a "$LOG_FILE" >&2
  exit 2
fi
echo "[smoke] binary: $SMOKE_BINARY" | tee -a "$LOG_FILE"

# Capture the three repo commits the report should pin so it stays
# auditable post-hoc. `|| echo unknown` so a missing repo doesn't fail
# the whole smoke.
PROTOCOL_COMMIT="$(git -C "$WORKSPACE_ROOT/privchat-protocol" rev-parse --short HEAD 2>/dev/null || echo unknown)"
SERVER_COMMIT="$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
APPLICATION_COMMIT="$(git -C "$MODULE_PRIVCHAT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
echo "[smoke] protocol_commit=$PROTOCOL_COMMIT" | tee -a "$LOG_FILE"
echo "[smoke] server_commit=$SERVER_COMMIT" | tee -a "$LOG_FILE"
echo "[smoke] application_commit=$APPLICATION_COMMIT" | tee -a "$LOG_FILE"

echo "[smoke] running Rust integration test ..." | tee -a "$LOG_FILE"
SMOKE_BINARY="$SMOKE_BINARY" \
PROTOCOL_COMMIT="$PROTOCOL_COMMIT" \
SERVER_COMMIT="$SERVER_COMMIT" \
APPLICATION_COMMIT="$APPLICATION_COMMIT" \
  cargo test \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --test e2e_channel_transfer_smoke \
    -- \
    --ignored --nocapture \
    2>&1 | tee -a "$LOG_FILE"

echo "[$(date '+%Y-%m-%d %H:%M:%S')] === Smoke complete ===" | tee -a "$LOG_FILE"
echo
echo "Reports:"
ls -1 "$REPORT_DIR" | sed "s|^|  $REPORT_DIR/|"
