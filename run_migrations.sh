#!/bin/bash
# 运行数据库迁移脚本

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

echo "🔧 运行数据库迁移..."
echo "📊 数据库: DATABASE_URL 已配置（不在日志中打印完整连接串）"

# 运行迁移文件。000_drop_all_tables.sql 是破坏性脚本，只允许人工显式执行。
migration_found=0
while IFS= read -r migration; do
    migration_found=1
    echo "📝 执行迁移: $migration"
    psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f "$migration" || {
        echo "❌ 迁移失败: $migration"
        exit 1
    }
done < <(find "$SCRIPT_DIR/migrations" -maxdepth 1 -type f -name '*.sql' ! -name '000_drop_all_tables.sql' | sort)

if [ "$migration_found" -eq 0 ]; then
    echo "❌ 未找到可执行迁移文件"
    exit 1
fi

echo "✅ 数据库迁移完成！"
