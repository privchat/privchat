#!/usr/bin/env bash
# P1-20 CI gate：privchat-server 合入前的统一门禁（本地与 CI 同一入口）。
#
#   bash scripts/ci_gate.sh
#
# 门禁内容（全部必须通过）：
#   1. cargo check --lib --bin privchat   —— 主目标编译
#   2. cargo check --all-targets          —— tests/examples 不再豁免（2026-07-09 起编译债清零）
#   3. cargo test --lib                   —— 单测（真库）
#   4. cargo test --tests                 —— 集成测试（真库 + 隔离 Redis）
#
# 真基础设施门禁（对齐 5ac8386 起的 fail-loud 契约）：
#   - require_test_database_url 缺 DB 默认 panic：「跳过记通过」是假绿，
#     所以本门禁不再提供无 DB 降级。必须提供已迁移的测试库：
#       DATABASE_URL / PRIVCHAT_TEST_DATABASE_URL → privchat migrate 过的库。
#   - assistant_direct_test 要求 PRIVCHAT_TEST_REDIS_URL 指向**隔离 Redis**
#     （不得复用 dev 实例）。本地示例：
#       redis-server --port 6390 --save '' --appendonly no --daemonize yes
#       export PRIVCHAT_TEST_REDIS_URL=redis://127.0.0.1:6390
#   - PRIVCHAT_ALLOW_SKIPPING_DB_TESTS 在门禁内强制清除：不允许降级刷绿。
#
# 明确不在门禁内：
#   - cargo fmt --all -- --check：历史格式债会在大量未改动文件上报 diff，
#     全量格式化会污染 blame。新代码用 rustfmt 保持整洁；全量收敛单独立项。
#   - #[ignore] 的 live-server / S3 live / call fixture 用例：需要运行中的
#     server、对象存储凭据或显式隔离 fixture，按各文件头注释单独跑。

set -euo pipefail
cd "$(dirname "$0")/.."

export RUSTC_WRAPPER=
unset PRIVCHAT_ALLOW_SKIPPING_DB_TESTS 2>/dev/null || true

if [[ -z "${PRIVCHAT_TEST_DATABASE_URL:-${DATABASE_URL:-}}" ]]; then
  echo "[gate] FATAL: 缺 DATABASE_URL / PRIVCHAT_TEST_DATABASE_URL。" >&2
  echo "[gate] 门禁是真库门禁：先建测试库并跑 privchat migrate，再导出连接串。" >&2
  exit 1
fi
if [[ -z "${PRIVCHAT_TEST_REDIS_URL:-}" ]]; then
  echo "[gate] FATAL: 缺 PRIVCHAT_TEST_REDIS_URL（隔离 Redis，见脚本头注释）。" >&2
  exit 1
fi

echo "[gate 1/4] cargo check --lib --bin privchat"
cargo check --quiet --lib --bin privchat

echo "[gate 2/4] cargo check --all-targets"
cargo check --quiet --all-targets

echo "[gate 3/4] cargo test --lib"
cargo test --quiet --lib

echo "[gate 4/4] cargo test --tests"
cargo test --quiet --tests

echo "[gate] ALL GREEN"