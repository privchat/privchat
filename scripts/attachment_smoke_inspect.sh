#!/usr/bin/env bash
# 附件加密本机 smoke 检查工具：发图后跑此脚本，校验
#   (a) 落盘文件是密文（前 12B 随机 nonce，不是 PNG/JPG/WEBP 魔数）
#   (b) privchat_file_uploads 最近记录 encryption_version=1 / cek 非空 / business 绑定
#
# 用法：
#   scripts/attachment_smoke_inspect.sh                 # 检查最近 5 分钟内修改的 blob + 最近 10 条 DB
#   DB_URL=... STORE=... scripts/attachment_smoke_inspect.sh
set -uo pipefail

STORE="${STORE:-./storage/files}"
DB_URL="${DB_URL:-${PRIVCHAT_TEST_DATABASE_URL:-${DATABASE_URL:-postgres://zoujiaqing@localhost:5432/privchat}}}"

echo "== storage_root: $STORE =="
echo "== db: ${DB_URL%%@*}@... =="
echo

magic_check() {
  local f="$1"
  # 读前 16 字节 hex
  local hex
  hex=$(xxd -p -l 16 "$f" 2>/dev/null | tr -d '\n' | tr 'a-f' 'A-F')
  local head8="${hex:0:8}"
  local head6="${hex:0:6}"
  local verdict="OK(ciphertext-like)"
  case "$head8" in
    89504E47) verdict="!!! PLAINTEXT PNG (89504E47) — encryption FAILED" ;;
    52494646) # RIFF — check WEBP at offset 8
      local webp; webp=$(xxd -p -s 8 -l 4 "$f" 2>/dev/null | tr -d '\n' | tr 'a-f' 'A-F')
      [ "$webp" = "57454250" ] && verdict="!!! PLAINTEXT WEBP (RIFF..WEBP) — encryption FAILED" ;;
  esac
  case "$head6" in
    FFD8FF) verdict="!!! PLAINTEXT JPG (FFD8FF) — encryption FAILED" ;;
  esac
  printf "  %-60s head=%s  %s\n" "$(basename "$f")" "$head8" "$verdict"
}

echo "== recently-modified blobs (images + thumbnails, last 10 min) =="
found=0
for dir in "$STORE/images" "$STORE/thumbnails" "$STORE/files" "$STORE/videos"; do
  [ -d "$dir" ] || continue
  while IFS= read -r f; do
    [ -z "$f" ] && continue
    found=1
    magic_check "$f"
  done < <(find "$dir" -type f -mmin -10 2>/dev/null | head -20)
done
[ "$found" = 0 ] && echo "  (no blobs modified in the last 10 min — send an attachment first)"
echo

echo "== privchat_file_uploads (latest 10) =="
psql "$DB_URL" -P pager=off -c \
"SELECT file_id, encryption_version AS enc_v,
        CASE WHEN cek IS NULL THEN '(null)' ELSE left(cek,6)||'…' END AS cek,
        business_type AS biz_type, business_id AS biz_id, file_type, uploaded_at
 FROM privchat_file_uploads
 ORDER BY uploaded_at DESC NULLS LAST, file_id DESC
 LIMIT 10;" 2>&1
echo

# 可选：MSG_ID=<message_id> 时，查该消息绑定的所有 file（Scheme B：主图 + 缩略图各一条）。
# 期望每条 enc_v=1 / cek 非空 / business_type=message / business_id=该 message_id。
if [ -n "${MSG_ID:-}" ]; then
  echo "== files bound to message_id=$MSG_ID (expect main + thumbnail, both enc_v=1) =="
  psql "$DB_URL" -P pager=off -c \
  "SELECT file_id, file_type, encryption_version AS enc_v,
          (cek IS NOT NULL) AS has_cek, business_type AS biz_type, business_id AS biz_id
   FROM privchat_file_uploads
   WHERE business_id = '$MSG_ID'
   ORDER BY file_id;" 2>&1
  cnt=$(psql "$DB_URL" -tAc "SELECT count(*) FROM privchat_file_uploads WHERE business_id='$MSG_ID';" 2>/dev/null | tr -d '[:space:]')
  bad=$(psql "$DB_URL" -tAc "SELECT count(*) FROM privchat_file_uploads WHERE business_id='$MSG_ID' AND (encryption_version<>1 OR cek IS NULL OR business_type<>'message');" 2>/dev/null | tr -d '[:space:]')
  echo "  bound files: $cnt   (image-with-thumbnail 应为 2)"
  if [ "${bad:-1}" = "0" ] && [ "${cnt:-0}" -ge 1 ]; then
    echo "  ✅ all bound files: enc_v=1, cek present, business_type=message, business_id=$MSG_ID"
  else
    echo "  ⚠️ $bad bound file(s) violate enc_v=1/cek/business_type — inspect above"
  fi
  echo
  echo "== privchat_messages.metadata for $MSG_ID (CEK 红线：绝不能出现 cek/thumbnail_cek) =="
  psql "$DB_URL" -P pager=off -x -c \
  "SELECT message_id, message_type, metadata::text AS metadata
   FROM privchat_messages WHERE message_id = '$MSG_ID';" 2>&1
  leak=$(psql "$DB_URL" -tAc "SELECT count(*) FROM privchat_messages WHERE message_id='$MSG_ID' AND metadata::text ~* '\"(thumbnail_)?cek\"';" 2>/dev/null | tr -d '[:space:]')
  if [ "${leak:-1}" = "0" ]; then
    echo "  ✅ metadata 不含 cek/thumbnail_cek（Scheme B：CEK 只在 file/get_url）"
  else
    echo "  ❌ metadata 含 cek/thumbnail_cek — CEK 泄进消息协议，违反 Scheme B"
  fi
  echo "  期望 metadata 含 file_id + thumbnail_file_id（typed 引用），不含任何 cek。"
fi
