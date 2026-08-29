#!/usr/bin/env bash
# 生成 PrivChat 服务端的长期自签证书（QUIC 与 TLS/TCP 共用）。
#
# 为什么必须落盘：msgtrans 的 `configure_server_insecure_with_config` 在每次进程启动时
# 现生成自签证书、只存在内存里，SPKI 每次重启都变，客户端无法 pinning
# （GATEWAY_TRANSPORT_SPEC §1.1）。
#
# 用法：
#   ./scripts/gen-server-tls.sh 106.55.63.153 /etc/privchat/tls
#   ./scripts/gen-server-tls.sh 127.0.0.1     ./certs        # 本地开发
#
# 生成后把路径填进 config.toml 的**网关级**配置（QUIC 与 TLS/TCP 共用同一身份）：
#   [gateway.tls]
#   cert = "/etc/privchat/tls/server.crt"
#   key  = "/etc/privchat/tls/server.key"
#
# ⚠️ listener 级的 tls_cert/tls_key 已废止，出现即拒绝启动。
#
# 🔴 私钥不进 Git；生产上属主为服务账号、权限 0600。

set -euo pipefail

HOST="${1:?用法: $0 <host-or-ip> [outdir]}"
OUTDIR="${2:-/etc/privchat/tls}"
DAYS="${DAYS:-3650}"

mkdir -p "$OUTDIR"
CRT="$OUTDIR/server.crt"
KEY="$OUTDIR/server.key"

if [[ -e "$CRT" || -e "$KEY" ]]; then
  echo "❌ 已存在证书，拒绝覆盖（覆盖会让所有已发布客户端的 pin 失效）:"
  echo "   $CRT"
  echo "   $KEY"
  echo "   确实要轮换请先按 spec 的轮换顺序走 current+next 双 pin，再手工移走旧文件。"
  exit 1
fi

# IP 与域名都要进 SAN：客户端 server_name 用 IP 时，CN 不生效，只认 SAN。
if [[ "$HOST" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  SAN="IP:$HOST"
else
  SAN="DNS:$HOST"
fi

openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout "$KEY" -out "$CRT" \
  -days "$DAYS" -subj "/CN=$HOST" \
  -addext "subjectAltName=$SAN" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature,keyEncipherment" \
  -addext "extendedKeyUsage=serverAuth" 2>/dev/null

chmod 600 "$KEY"
chmod 644 "$CRT"

# 属主必须是跑服务的账号。用 sudo 生成时文件会归 root，systemd 下的服务账号读不到，
# 服务端会以「权限/读取失败」拒绝启动（这是有意的 fail-closed，不是 bug）。
# 设 SERVICE_USER 即自动 chown；否则只提示。
if [[ -n "${SERVICE_USER:-}" ]]; then
  chown "$SERVICE_USER:$SERVICE_USER" "$KEY" "$CRT"
  echo "👤 属主已设为 $SERVICE_USER"
elif [[ "$OUTDIR" == /etc/* ]]; then
  echo "⚠️  部署到系统目录但未设 SERVICE_USER。服务端启动前请执行："
  echo "    chown <service-user>:<service-user> $KEY && chmod 600 $KEY"
fi

echo "✅ 已生成（有效期 ${DAYS} 天）"
echo "   证书: $CRT"
echo "   私钥: $KEY  (0600)"
echo
echo "SPKI SHA-256 (base64) —— 填进客户端 brand profile 的 pin 列表:"
openssl x509 -in "$CRT" -pubkey -noout \
  | openssl pkey -pubin -outform der \
  | openssl dgst -sha256 -binary \
  | openssl enc -base64
