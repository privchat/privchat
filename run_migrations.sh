#!/bin/bash
# 运行数据库迁移脚本

set -e

# 从 .env 文件读取数据库连接字符串
if [ -f .env ]; then
    export $(cat .env | grep -v '^#' | xargs)
fi

# 默认数据库连接字符串
DATABASE_URL=${DATABASE_URL:-"postgres://zoujiaqing@localhost:5432/privchat"}

echo "🔧 运行数据库迁移..."
echo "📊 数据库: $DATABASE_URL"

# 运行迁移文件
for migration in migrations/*.sql; do
    if [ -f "$migration" ]; then
        echo "📝 执行迁移: $migration"
        psql "$DATABASE_URL" -f "$migration" || {
            echo "❌ 迁移失败: $migration"
            exit 1
        }
    fi
done

echo "✅ 数据库迁移完成！"
