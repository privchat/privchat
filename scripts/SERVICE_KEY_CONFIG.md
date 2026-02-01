# X-Service-Key 配置说明

## 配置位置

X-Service-Key 通过环境变量 `SERVICE_MASTER_KEY` 配置。

## 配置方式

### 方式 1: 使用 .env 文件（推荐）

编辑 `privchat-server/.env` 文件，取消注释并设置：

```bash
SERVICE_MASTER_KEY=your-actual-service-master-key-here
```

### 方式 2: 使用环境变量

```bash
export SERVICE_MASTER_KEY="your-actual-service-master-key-here"
```

### 方式 3: 启动时设置

```bash
SERVICE_MASTER_KEY="your-actual-service-master-key-here" ./target/debug/privchat-server
```

## 默认值

如果未设置 `SERVICE_MASTER_KEY`，服务器会使用默认值：
```
default-service-master-key-please-change-in-production
```

⚠️ **注意**: 生产环境请务必修改为强密码！

## 验证配置

启动服务器后，查看日志中是否有：
```
✅ Service Key 管理器初始化完成
```

## 在测试脚本中使用

测试脚本 `test_admin_api.py` 默认使用默认值。如果需要使用自定义密钥：

1. 修改脚本中的 `SERVICE_KEY` 变量
2. 或使用 `--service-key` 参数：
   ```bash
   python3 scripts/test_admin_api.py --service-key "your-actual-service-key"
   ```
3. 或使用配置文件：
   ```bash
   python3 scripts/test_admin_api.py --config scripts/config.json
   ```

## 安全建议

1. **生产环境**: 使用强随机字符串作为 Service Master Key
2. **不要提交**: 将 `.env` 文件添加到 `.gitignore`
3. **定期轮换**: 定期更换 Service Master Key
4. **最小权限**: 只为需要的系统分配 Service Key
