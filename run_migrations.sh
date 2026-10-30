#!/bin/bash
# Use the binary's checksum ledger, locking and transactions, never raw psql.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATABASE_URL_OVERRIDE="${DATABASE_URL-}"
if [ -f "$SCRIPT_DIR/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    source "$SCRIPT_DIR/.env"
    set +a
fi
if [ -n "$DATABASE_URL_OVERRIDE" ]; then
    export DATABASE_URL="$DATABASE_URL_OVERRIDE"
fi
: "${DATABASE_URL:?Set DATABASE_URL in the environment or .env}"
export DATABASE_URL

PRIVCHAT_BIN="${PRIVCHAT_BIN:-$SCRIPT_DIR/target/release/privchat}"
if [ ! -x "$PRIVCHAT_BIN" ]; then
    echo "Build the current release first, or set PRIVCHAT_BIN to its executable." >&2
    exit 1
fi
exec "$PRIVCHAT_BIN" migrate
