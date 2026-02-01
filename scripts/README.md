# 管理 API 测试脚本

## 简介

提供了两个版本的测试脚本：
- **Python 版本** (`test_admin_api.py`) - 推荐使用，功能完整
- **Bash 版本** (`test_admin_api.sh`) - 轻量级，快速测试

## 功能

- ✅ 创建用户
- ✅ 获取用户列表和详情
- ✅ 创建好友关系
- ✅ 获取好友关系列表
- ✅ 获取用户的好友列表
- ✅ 获取群组列表和详情
- ✅ 获取系统统计信息

## Python 版本 (推荐)

### 依赖

```bash
pip install -r scripts/requirements.txt
# 或
pip install requests
```

### 优点

- ✅ 完整的错误处理和测试报告
- ✅ 彩色输出，易于阅读
- ✅ 支持配置文件
- ✅ 详细的测试结果统计
- ✅ 易于扩展和维护

### 使用方法

见下方详细说明。

## 使用方法

### 方法 1: 直接运行（使用默认配置）

```bash
# 1. 修改脚本中的 SERVICE_KEY
vim scripts/test_admin_api.py
# 找到 SERVICE_KEY = "your-service-key-here" 并替换为实际的 service key

# 2. 运行测试
python3 scripts/test_admin_api.py
```

### 方法 2: 使用命令行参数

```bash
python3 scripts/test_admin_api.py \
  --url http://localhost:8080 \
  --service-key your-actual-service-key
```

### 方法 3: 使用配置文件

```bash
# 1. 复制示例配置文件
cp scripts/config.example.json scripts/config.json

# 2. 编辑配置文件
vim scripts/config.json
# 填入实际的 base_url 和 service_key

# 3. 运行测试
python3 scripts/test_admin_api.py --config scripts/config.json
```

## 测试流程

脚本会按以下顺序执行测试：

1. **创建用户** - 创建 4 个测试用户
2. **获取用户列表** - 验证用户列表接口
3. **获取用户详情** - 验证用户详情接口
4. **创建好友关系** - 创建多个好友关系
5. **获取好友关系列表** - 验证好友关系列表接口
6. **获取用户的好友列表** - 验证用户好友列表接口
7. **获取群组列表** - 验证群组列表接口
8. **获取系统统计** - 验证统计接口

## 输出示例

```
============================================================
开始运行管理 API 测试
============================================================

--- 测试 1: 创建用户 ---
ℹ️  创建用户: username=test_user_1
✅ 用户创建成功: user_id=1, username=test_user_1
...

============================================================
测试总结
============================================================

总测试数: 15
通过: 15
失败: 0

创建的资源:
  - 用户: 4 个
  - 好友关系: 4 个
  - 群组: 0 个

============================================================
```

## Bash 版本

### 依赖

- `curl` (必需)
- `jq` (可选，用于格式化 JSON 输出)

### 优点

- ✅ 无需安装 Python 依赖
- ✅ 轻量级，快速执行
- ✅ 适合简单的自动化脚本

### 使用方法

```bash
# 设置环境变量
export BASE_URL="http://localhost:8080"
export SERVICE_KEY="your-actual-service-key"

# 运行测试
./scripts/test_admin_api.sh
```

## 两种版本对比

| 特性 | Python 版本 | Bash 版本 |
|------|------------|-----------|
| 错误处理 | ✅ 完整 | ⚠️ 基础 |
| 测试报告 | ✅ 详细统计 | ❌ 无 |
| 彩色输出 | ✅ 是 | ✅ 是 |
| 配置文件支持 | ✅ 是 | ❌ 否 |
| 依赖要求 | requests | curl, jq(可选) |
| 扩展性 | ✅ 高 | ⚠️ 中 |
| 推荐场景 | 完整测试、CI/CD | 快速验证、简单脚本 |

## 扩展测试

如果需要测试更多功能，可以修改相应的测试方法。

### Python 版本

修改 `run_all_tests()` 方法，添加更多测试用例：

```python
def test_create_group(self, owner_id: int, name: str) -> Optional[int]:
    """测试创建群组（需要先实现创建群组的接口）"""
    # TODO: 实现创建群组的逻辑
    pass
```

### Bash 版本

添加新的测试函数：

```bash
test_create_group() {
    local owner_id=$1
    local name=$2
    # 实现创建群组的逻辑
}
```

## 注意事项

1. 确保服务器已启动并运行在指定的地址
2. 确保 SERVICE_KEY 正确配置
3. 测试会创建真实的用户和关系，建议在测试环境中运行
4. 如果需要清理测试数据，可以手动删除或添加清理功能
5. Python 版本需要 Python 3.6+
6. Bash 版本需要 Bash 4.0+（macOS 默认版本可能较旧，建议使用 Python 版本）
