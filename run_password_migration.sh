#!/bin/bash
# 执行密码字段迁移脚本
# 
# 使用方法：
# 1. 确保 PostgreSQL 正在运行
# 2. 设置 DATABASE_URL 环境变量（如果需要）
# 3. 运行此脚本: ./run_password_migration.sh

set -e

echo "🔧 开始执行数据库迁移..."
echo ""

# 从环境变量或默认值获取数据库连接信息
DB_URL="${DATABASE_URL:-postgres://postgres:postgres@localhost:5432/privchat}"

echo "📊 数据库连接: $DB_URL"
echo ""

# 解析数据库 URL
DB_USER=$(echo $DB_URL | sed -n 's|.*://\([^:]*\):.*|\1|p')
DB_PASS=$(echo $DB_URL | sed -n 's|.*://[^:]*:\([^@]*\)@.*|\1|p')
DB_HOST=$(echo $DB_URL | sed -n 's|.*@\([^:]*\):.*|\1|p')
DB_PORT=$(echo $DB_URL | sed -n 's|.*:\([0-9]*\)/.*|\1|p')
DB_NAME=$(echo $DB_URL | sed -n 's|.*/\([^?]*\).*|\1|p')

echo "📋 连接信息:"
echo "  - 主机: $DB_HOST"
echo "  - 端口: $DB_PORT"
echo "  - 数据库: $DB_NAME"
echo "  - 用户: $DB_USER"
echo ""

# 检查数据库是否可连接
echo "🔍 检查数据库连接..."
if PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER -d $DB_NAME -c "SELECT 1" > /dev/null 2>&1; then
    echo "✅ 数据库连接成功"
else
    echo "❌ 数据库连接失败，请检查："
    echo "  1. PostgreSQL 是否正在运行"
    echo "  2. 数据库连接信息是否正确"
    echo "  3. 数据库是否已创建"
    exit 1
fi

echo ""
echo "📝 执行迁移脚本: migrations/002_add_password_hash.sql"
echo ""

# 执行迁移
PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER -d $DB_NAME -f migrations/002_add_password_hash.sql

if [ $? -eq 0 ]; then
    echo ""
    echo "✅ 迁移执行成功！"
    echo ""
    echo "📊 验证迁移结果..."
    PGPASSWORD=$DB_PASS psql -h $DB_HOST -p $DB_PORT -U $DB_USER -d $DB_NAME -c "\d privchat_users" | grep password_hash
    echo ""
    echo "🎉 密码字段已成功添加到 privchat_users 表！"
else
    echo ""
    echo "❌ 迁移执行失败"
    exit 1
fi
