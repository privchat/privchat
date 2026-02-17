#!/bin/bash
# pre-loadtest-check.sh — 压测前保护开关验证
# 参考: STABILITY_SPEC 第 9 节
#
# 用法: ./scripts/pre-loadtest-check.sh [config.toml路径]

set -e

echo "=== 压测前保护开关检查 ==="
echo ""

CONFIG=${1:-config.toml}

if [ ! -f "$CONFIG" ]; then
    echo "FAIL: 配置文件 '$CONFIG' 不存在"
    exit 1
fi

FAILED=0

check_toml_key() {
    local key=$1
    local desc=$2
    # 支持 TOML 嵌套 key（如 [cache.redis] 下的 command_timeout_ms）
    local value
    value=$(grep -E "^\s*${key}\s*=" "$CONFIG" 2>/dev/null | tail -1 | cut -d'=' -f2 | tr -d ' "')
    if [ -z "$value" ] || [ "$value" = "0" ] || [ "$value" = "false" ]; then
        echo "  FAIL: $key = '$value' ($desc)"
        FAILED=1
    else
        echo "  PASS: $key = $value"
    fi
}

echo "[1/4] 资源边界检查"
check_toml_key "command_timeout_ms" "Redis 命令超时，必须 > 0"
check_toml_key "pool_size" "Redis 连接池大小，必须 > 0"
check_toml_key "handler_max_inflight" "Handler 并发限流，必须 > 0"
echo ""

echo "[2/4] Cargo.toml panic 策略检查"
CARGO_TOML="Cargo.toml"
if [ ! -f "$CARGO_TOML" ]; then
    echo "  SKIP: $CARGO_TOML 不存在（非项目根目录）"
else
    if grep -q 'panic.*=.*"abort"' "$CARGO_TOML" 2>/dev/null; then
        echo "  PASS: panic = \"abort\""
    else
        echo "  FAIL: Cargo.toml 未设置 panic = \"abort\""
        echo "        压测要求 [profile.release] panic = \"abort\""
        FAILED=1
    fi
fi
echo ""

echo "[3/4] RUST_LOG 级别检查"
if [ -z "$RUST_LOG" ]; then
    echo "  WARN: RUST_LOG 未设置，将使用默认级别"
    echo "        建议: export RUST_LOG=info"
elif echo "$RUST_LOG" | grep -qE "^(error|warn)$"; then
    echo "  FAIL: RUST_LOG='$RUST_LOG'（压测要求至少 info，禁止 error-only）"
    FAILED=1
else
    echo "  PASS: RUST_LOG=$RUST_LOG"
fi
echo ""

echo "[4/4] 环境变量检查"
if [ -n "$LOAD_TEST_ENV" ]; then
    echo "  PASS: LOAD_TEST_ENV=$LOAD_TEST_ENV"
else
    echo "  WARN: LOAD_TEST_ENV 未设置（默认 local 阈值）"
    echo "        CI 环境建议: export LOAD_TEST_ENV=ci"
fi
echo ""

echo "========================================="
if [ $FAILED -eq 1 ]; then
    echo "  检查未通过，请修复配置后再启动压测"
    exit 1
fi
echo "  全部通过，可以开始压测"
echo "========================================="
