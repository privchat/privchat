#!/bin/bash
# 检查并运行数据库迁移

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# 从 .env 文件读取数据库连接字符串
if [ -f "$SCRIPT_DIR/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    source "$SCRIPT_DIR/.env"
    set +a
fi

# 默认数据库连接字符串
DATABASE_URL=${DATABASE_URL:-"postgres://zoujiaqing@localhost:5432/privchat"}

echo "🔍 检查数据库迁移状态..."

# 检查 privchat_users 表是否存在
TABLE_EXISTS=$(psql "$DATABASE_URL" -tAc "SELECT EXISTS (SELECT FROM information_schema.tables WHERE table_schema = 'public' AND table_name = 'privchat_users');" 2>/dev/null || echo "false")

if [ "$TABLE_EXISTS" = "t" ]; then
    echo "✅ 数据库表已存在，跳过迁移"
else
    echo "📝 数据库表不存在，开始运行迁移..."
    "$SCRIPT_DIR/run_migrations.sh"
    echo "✅ 数据库迁移完成！"
fi
