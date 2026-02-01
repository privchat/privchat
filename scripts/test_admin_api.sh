#!/bin/bash
# 管理 API 测试脚本 (Bash 版本)
# 
# 这是一个简单的 bash 版本，用于快速测试管理接口
# 推荐使用 Python 版本 (test_admin_api.py) 以获得更好的功能和错误处理

set -e

# 配置
BASE_URL="${BASE_URL:-http://localhost:8080}"
SERVICE_KEY="${SERVICE_KEY:-your-service-key-here}"

# 颜色输出
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

log_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

log_error() {
    echo -e "${RED}❌ $1${NC}"
}

log_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

# 发送 HTTP 请求
make_request() {
    local method=$1
    local endpoint=$2
    local data=$3
    
    local url="${BASE_URL}${endpoint}"
    local headers=(
        -H "X-Service-Key: ${SERVICE_KEY}"
        -H "Content-Type: application/json"
    )
    
    if [ "$method" = "GET" ]; then
        curl -s -w "\n%{http_code}" "${headers[@]}" "$url" ${data:+--data-urlencode "$data"}
    elif [ "$method" = "POST" ]; then
        curl -s -w "\n%{http_code}" "${headers[@]}" -d "$data" "$url"
    elif [ "$method" = "PUT" ]; then
        curl -s -w "\n%{http_code}" "${headers[@]}" -X PUT -d "$data" "$url"
    elif [ "$method" = "DELETE" ]; then
        curl -s -w "\n%{http_code}" "${headers[@]}" -X DELETE "$url"
    fi
}

# 测试创建用户
test_create_user() {
    local username=$1
    local display_name=$2
    local email=$3
    
    log_info "创建用户: username=${username}"
    
    local data=$(cat <<EOF
{
    "username": "${username}",
    "display_name": "${display_name}",
    "email": "${email}"
}
EOF
)
    
    local response=$(make_request "POST" "/api/admin/users" "$data")
    local http_code=$(echo "$response" | tail -n1)
    local body=$(echo "$response" | sed '$d')
    
    if [ "$http_code" = "200" ]; then
        local user_id=$(echo "$body" | grep -o '"user_id":[0-9]*' | grep -o '[0-9]*')
        log_success "用户创建成功: user_id=${user_id}"
        echo "$user_id"
    else
        log_error "创建用户失败: HTTP ${http_code}"
        echo "$body" | jq '.' 2>/dev/null || echo "$body"
        echo ""
    fi
}

# 测试获取用户列表
test_list_users() {
    log_info "获取用户列表"
    
    local response=$(make_request "GET" "/api/admin/users?page=1&page_size=10")
    local http_code=$(echo "$response" | tail -n1)
    local body=$(echo "$response" | sed '$d')
    
    if [ "$http_code" = "200" ]; then
        local total=$(echo "$body" | jq -r '.total // 0' 2>/dev/null || echo "0")
        log_success "获取用户列表成功: 总数=${total}"
        echo "$body" | jq '.' 2>/dev/null || echo "$body"
    else
        log_error "获取用户列表失败: HTTP ${http_code}"
        echo "$body"
    fi
}

# 测试创建好友关系
test_create_friendship() {
    local user1_id=$1
    local user2_id=$2
    
    log_info "创建好友关系: ${user1_id} <-> ${user2_id}"
    
    local data=$(cat <<EOF
{
    "user1_id": ${user1_id},
    "user2_id": ${user2_id}
}
EOF
)
    
    local response=$(make_request "POST" "/api/admin/friendships" "$data")
    local http_code=$(echo "$response" | tail -n1)
    local body=$(echo "$response" | sed '$d')
    
    if [ "$http_code" = "200" ]; then
        log_success "好友关系创建成功"
        echo "$body" | jq '.' 2>/dev/null || echo "$body"
    else
        log_error "创建好友关系失败: HTTP ${http_code}"
        echo "$body" | jq '.' 2>/dev/null || echo "$body"
    fi
}

# 测试获取系统统计
test_get_stats() {
    log_info "获取系统统计"
    
    local response=$(make_request "GET" "/api/admin/stats")
    local http_code=$(echo "$response" | tail -n1)
    local body=$(echo "$response" | sed '$d')
    
    if [ "$http_code" = "200" ]; then
        log_success "获取系统统计成功"
        echo "$body" | jq '.' 2>/dev/null || echo "$body"
    else
        log_error "获取系统统计失败: HTTP ${http_code}"
        echo "$body"
    fi
}

# 主函数
main() {
    echo "============================================================"
    echo "管理 API 测试脚本 (Bash 版本)"
    echo "============================================================"
    echo ""
    
    if [ "$SERVICE_KEY" = "your-service-key-here" ]; then
        log_warning "请设置 SERVICE_KEY 环境变量"
        echo "   export SERVICE_KEY='your-actual-service-key'"
        exit 1
    fi
    
    # 检查依赖
    if ! command -v curl &> /dev/null; then
        log_error "需要安装 curl"
        exit 1
    fi
    
    if ! command -v jq &> /dev/null; then
        log_warning "建议安装 jq 以获得更好的 JSON 输出"
    fi
    
    # 运行测试
    echo ""
    echo "--- 测试 1: 创建用户 ---"
    user1_id=$(test_create_user "test_user_1" "测试用户1" "user1@test.com")
    user2_id=$(test_create_user "test_user_2" "测试用户2" "user2@test.com")
    
    if [ -z "$user1_id" ] || [ -z "$user2_id" ]; then
        log_error "用户创建失败，终止测试"
        exit 1
    fi
    
    echo ""
    echo "--- 测试 2: 获取用户列表 ---"
    test_list_users
    
    echo ""
    echo "--- 测试 3: 创建好友关系 ---"
    test_create_friendship "$user1_id" "$user2_id"
    
    echo ""
    echo "--- 测试 4: 获取系统统计 ---"
    test_get_stats
    
    echo ""
    log_success "测试完成！"
}

main "$@"
