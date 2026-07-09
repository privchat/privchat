#!/usr/bin/env bash
# P1-20 CI gate：privchat-server 合入前的统一门禁（本地与 CI 同一入口）。
#
#   bash scripts/ci_gate.sh
#
# 门禁内容（全部必须通过）：
#   1. cargo check --lib --bin privchat   —— 主目标编译
#   2. cargo check --all-targets          —— tests/examples 不再豁免（2026-07-09 起编译债清零）
#   3. cargo test --lib                   —— 单测（无 DB 环境；DB-in-loop 测试自动 skip）
#   4. cargo test --tests                 —— 集成测试（同上）
#
# 明确不在门禁内：
#   - cargo fmt --all -- --check：历史格式债会在大量未改动文件上报 diff，
#     全量格式化会污染 blame。新代码用 rustfmt 保持整洁；全量收敛单独立项。
#   - DB-in-loop 测试：依赖 DATABASE_URL 与共享 dev 库状态，不适合无状态门禁；
#     本脚本显式清空 DATABASE_URL 让它们走 skip 分支。

set -euo pipefail
cd "$(dirname "$0")/.."

export RUSTC_WRAPPER=
unset DATABASE_URL PRIVCHAT_TEST_DATABASE_URL 2>/dev/null || true

echo "[gate 1/4] cargo check --lib --bin privchat"
cargo check --quiet --lib --bin privchat

echo "[gate 2/4] cargo check --all-targets"
cargo check --quiet --all-targets

echo "[gate 3/4] cargo test --lib"
cargo test --quiet --lib

echo "[gate 4/4] cargo test --tests"
cargo test --quiet --tests

echo "[gate] ALL GREEN"
