#!/bin/bash
# The ledger, not the existence of one table, determines pending work.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/run_migrations.sh"
