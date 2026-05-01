# RS256 Unified Token Keys

Spec: `privchat-docs/spec/05-feature/TOKEN_UNIFICATION_SPEC.md` §4.4 / §11.1

## ⚠️ 不允许提交密钥到 repo

`.gitignore` 已经过滤 `*.pem` / `*.key` / `/keys/` 目录里除本 README 外的所有文件。
本目录**仅**保存生成命令说明；真实私钥永远放在 repo 外（本地用 `./.local/keys/jwt/`，
生产用 `/etc/privchat/keys/jwt/`）。

## 生成 RSA 密钥对（开发用）

```bash
mkdir -p ./.local/keys/jwt
cd ./.local/keys/jwt

# 2048 位 RSA 私钥
openssl genrsa -out v1.pem 2048

# 导出公钥
openssl rsa -in v1.pem -pubout -out v1.pub.pem

# 检查
openssl rsa -in v1.pem -noout -text | head
openssl rsa -in v1.pub.pem -pubin -noout -text | head
```

## 配置

把路径配进 `privchat-server/config.toml` 的 `[auth.rsa_jwt]`，或通过环境变量：

```bash
export JWT_RS256_PRIVATE_KEY_PATH="$(pwd)/.local/keys/jwt/v1.pem"
export JWT_RS256_PUBLIC_KEY_PATH="$(pwd)/.local/keys/jwt/v1.pub.pem"
export JWT_RS256_KID="v1"
```

## 生产部署

| 项 | 路径 |
|----|------|
| 私钥 | `/etc/privchat/keys/jwt/v<N>.pem`，权限 0600，owner 是 server 进程 user |
| 公钥 | `/etc/privchat/keys/jwt/v<N>.pub.pem`，权限 0644 |
| kid | 与文件名 v<N> 对齐，每次轮换新增 v2 / v3 ... |

## 轮换流程

见 spec `TOKEN_UNIFICATION_SPEC.md` §11.1 标准轮换流程。简言之：

1. 生成 v<N+1> 密钥对放到 `keys/` 目录
2. JWKS endpoint 同时返回 v<N> + v<N+1>
3. 等 application JWKS 缓存全部刷新（≥ 1h）
4. 切签名 key 到 v<N+1>
5. v<N> 在 grace window 内仍可用于验签
6. v<N> 签的所有 token 过期后，从 JWKS 摘除 v<N> 并删除私钥文件

紧急轮换（密钥泄露）参见 spec §11.1。
