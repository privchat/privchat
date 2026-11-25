#!/usr/bin/env bash
# Requires isolated empty PostgreSQL and locally installed Redis, MinIO and aws CLI.
# Full mode also requires Prometheus and Alertmanager binaries (override with env).
set -euo pipefail
server=$(realpath "${1:?pass the Server binary}")
mode=${2:-full}
case "$mode" in full|metrics-only) ;; *) exit 2 ;; esac
repo=$(cd "$(dirname "$0")/../.." && pwd)
: "${DATABASE_URL:?set the isolated database URL}"
[[ "$DATABASE_URL" =~ ^postgres(ql)?://([^/@]+@)?(127\.0\.0\.1|localhost)(:[0-9]+)?/privchat_sd06_[A-Za-z0-9_]+$ ]]
[[ "$(psql -XAt -v ON_ERROR_STOP=1 "$DATABASE_URL" -c "SELECT count(*) FROM pg_tables WHERE schemaname='public'")" = 0 ]]
umask 077
root=$(mktemp -d "${TMPDIR:-/tmp}/privchat-observability.XXXXXX")
echo "Observability gate artifacts: $root"
cd "$root"
redis_pid= storage_pid= server_pid= prom_pid= alert_pid= webhook_pid=
cleanup() {
    local pid
    for pid in "$redis_pid" "$storage_pid" "$server_pid" "$prom_pid" "$alert_pid" "$webhook_pid"; do
        if [[ -n "$pid" ]]; then
            kill -CONT "$pid" 2>/dev/null || true
            kill "$pid" 2>/dev/null || true
        fi
    done
    wait || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
read -r tcp ws admin file redis s3 console prom alert webhook < <(python3 - <<'PY'
import socket
sockets = [socket.socket() for _ in range(10)]
for sock in sockets:
    sock.bind(("127.0.0.1", 0))
print(*(sock.getsockname()[1] for sock in sockets))
PY
)

wait_for() {
    local seconds=$1
    shift
    local end=$((SECONDS + seconds))
    until "$@"; do
        if (( SECONDS >= end )); then echo "Timeout: $*" >&2; return 1; fi
        sleep 1
    done
}
http() { curl --noproxy 127.0.0.1 -fsS --max-time 2 "$@" 2>/dev/null; }
export REDIS_URL="redis://127.0.0.1:$redis" SERVICE_MASTER_KEY
SERVICE_MASTER_KEY=$(openssl rand -hex 32)
redis-server --bind 127.0.0.1 --port "$redis" --save '' --appendonly no >redis.log 2>&1 &
redis_pid=$!
wait_for 15 redis-cli -p "$redis" ping >/dev/null

export MINIO_ROOT_USER=privchat-sd06 MINIO_ROOT_PASSWORD=isolated-fixture-only
mkdir objects
minio server "$root/objects" --address "127.0.0.1:$s3" --console-address "127.0.0.1:$console" >minio.log 2>&1 &
storage_pid=$!
wait_for 30 http -o /dev/null "http://127.0.0.1:$s3/minio/health/ready"
AWS_ACCESS_KEY_ID="$MINIO_ROOT_USER" AWS_SECRET_ACCESS_KEY="$MINIO_ROOT_PASSWORD" \
    AWS_DEFAULT_REGION=us-east-1 AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null \
    aws --endpoint-url "http://127.0.0.1:$s3" s3api create-bucket --bucket sd06 >/dev/null

"$server" generate-config base.toml
awk -v tcp="$tcp" -v ws="$ws" -v admin="$admin" -v file="$file" -v redis="$REDIS_URL" -v s3="$s3" '
    /^\[server_event\]/ { skip=1; next }
    /^\[/ { skip=0 }
    skip { next }
    { gsub(/0\.0\.0\.0/, "127.0.0.1") }
    /^port = 9001$/ { $0="port = " tcp }
    /^port = 9080$/ { $0="port = " ws }
    /^port = 9090$/ { print "host = \"127.0.0.1\""; $0="port = " admin }
    /^url = "redis:/ { $0="url = \"" redis "\"" }
    /^server_port = 9083$/ { $0="server_port = " file }
    /server_api_base_url/ { sub(/:9083/, ":" file) }
    /^storage_type = "local"$/ {
        print "storage_type = \"s3\""
        print "endpoint = \"http://127.0.0.1:" s3 "\""
        print "bucket = \"sd06\""
        print "access_key_id = \"privchat-sd06\""
        print "secret_access_key = \"isolated-fixture-only\""
        next
    }
    { print }
' base.toml >config.toml
"$server" --config-file "$root/config.toml" migrate >migrate.log 2>&1
"$server" --config-file "$root/config.toml" >server.log 2>&1 &
server_pid=$!

ready() { http -o /dev/null -H "X-Service-Key: $SERVICE_MASTER_KEY" "http://127.0.0.1:$admin/api/service/system/ready"; }
scrape() { http "http://127.0.0.1:$file/metrics" >snapshot; }
metric() { awk -v pattern="$1" '$1 ~ pattern { print $NF }' snapshot; }
dep() { metric "^privchat_dependency_up\\{dependency=\"$1\"\\}$"; }
healthy_pair() { scrape && [[ "$(dep redis)" = "$1" && "$(dep object_storage)" = "$2" ]]; }
wait_for 90 ready
wait_for 25 healthy_pair 1 1

if [[ "$mode" = full ]]; then
    cat >alertmanager.yml <<YAML
route:
  receiver: sd06-acceptance
  group_by: [alertname, dependency, instance]
  group_wait: 0s
  group_interval: 1s
  repeat_interval: 1h
receivers:
  - name: sd06-acceptance
    webhook_configs:
      - url: http://127.0.0.1:$webhook/alert
        send_resolved: true
YAML
    cat >prometheus.yml <<YAML
global:
  scrape_interval: 1s
  evaluation_interval: 1s
rule_files:
  - $repo/deploy/prometheus/dependencies.rules.yml
alerting:
  alertmanagers:
    - static_configs:
        - targets: ["127.0.0.1:$alert"]
scrape_configs:
  - job_name: privchat-sd06
    static_configs:
      - targets: ["127.0.0.1:$file"]
YAML
    python3 "$repo/scripts/acceptance/observability-webhook.py" "$webhook" "$root/events.log" >webhook.log 2>&1 &
    webhook_pid=$!
    wait_for 10 test -f events.ready
    "${ALERTMANAGER_BIN:-alertmanager}" --config.file=alertmanager.yml --storage.path="$root/alert-data" \
        --web.listen-address="127.0.0.1:$alert" --cluster.listen-address= >alertmanager.log 2>&1 &
    alert_pid=$!
    wait_for 20 http -o /dev/null "http://127.0.0.1:$alert/-/ready"
    "${PROMETHEUS_BIN:-prometheus}" --config.file=prometheus.yml --storage.tsdb.path="$root/prom-data" \
        --web.listen-address="127.0.0.1:$prom" >prometheus.log 2>&1 &
    prom_pid=$!
    wait_for 20 http -o /dev/null "http://127.0.0.1:$prom/-/ready"
fi

prom_result() {
    http -G "http://127.0.0.1:$prom/api/v1/query" --data-urlencode "query=$1" |
        python3 -c 'import json, sys; p = json.load(sys.stdin); sys.exit(p["status"] != "success" or len(p["data"]["result"]) != 1)'
}

# Only an isolated delayed row: no messages sent, no DEAD recovery invoked.
psql -Xq -v ON_ERROR_STOP=1 "$DATABASE_URL" <<'SQL'
BEGIN;
INSERT INTO privchat_users (user_id, username, qr_key) VALUES (990006001, 'sd06fixture', 'sd06fixture');
INSERT INTO privchat_commit_log (id, pts, server_msg_id, channel_id, channel_type, message_type, content, server_timestamp, sender_id)
  VALUES (990006001, 1, 990006001, 990006001, 1, 'text', '{}', now_millis(), 990006001);
INSERT INTO privchat_message_dispatch_outbox (event_id, channel_id, channel_type, pts, sender_id, event_kind, membership_version, created_at)
  VALUES (990006001, 990006001, 0, 1, 990006001, 1, 1, now_millis() - 12000);
INSERT INTO privchat_message_dispatch_recipient (event_id, user_id, next_attempt_at)
  VALUES (990006001, 990006001, now_millis() + 3600000);
COMMIT;
SQL
backlog() {
    scrape && [[ "$(metric '^privchat_timeline_dispatch_backlog_sample_ok$')" = 1 &&
        "$(metric '^privchat_timeline_dispatch_pending_events$')" = "$1" ]]
}
wait_for 25 backlog 1
awk '/^privchat_timeline_dispatch_oldest_pending_seconds / && $2 >= 12 { ok=1 } END { exit !ok }' snapshot
if [[ "$mode" = full ]]; then
    wait_for 10 prom_result 'privchat_timeline_dispatch_pending_events{job="privchat-sd06"} == 1'
    wait_for 10 prom_result 'privchat_timeline_dispatch_oldest_pending_seconds{job="privchat-sd06"} >= 12'
fi
psql -Xq -v ON_ERROR_STOP=1 "$DATABASE_URL" -c 'DELETE FROM privchat_commit_log WHERE id=990006001'
wait_for 25 backlog 0
[[ "$(metric '^privchat_timeline_dispatch_oldest_pending_seconds$')" = 0 ]]
if [[ "$mode" = full ]]; then
    wait_for 10 prom_result 'privchat_timeline_dispatch_pending_events{job="privchat-sd06"} == 0'
fi
echo '[PASS] durable backlog and oldest age rise, then return to zero'

event() { test -f events.log && grep -Eq "^$1 $2 $3$" events.log; }
total() {
    awk -v key="$1" -v dep="$2" '
        index($1,key)==1 && $1 ~ ("dependency=\"" dep "\"") && $1 !~ /result="ok"/ { n+=$NF }
        END { print n+0 }
    ' snapshot
}
exercise() {
    local dependency=$1 pid=$2 redis_up=$3 s3_up=$4
    local before count_before fingerprint
    scrape
    before=$(total 'privchat_dependency_probes_total{' "$dependency")
    count_before=$(total 'privchat_dependency_probe_duration_seconds_count{' "$dependency")
    kill -STOP "$pid"
    wait_for 25 healthy_pair "$redis_up" "$s3_up"
    ready
    [[ "$(total 'privchat_dependency_probes_total{' "$dependency")" -gt "$before" ]]
    [[ "$(total 'privchat_dependency_probe_duration_seconds_count{' "$dependency")" -gt "$count_before" ]]
    echo "[PASS] $dependency fault is isolated, error and latency metrics update while PG readiness stays green"
    if [[ "$mode" = full ]]; then
        wait_for 10 prom_result "sum(privchat_dependency_probes_total{job=\"privchat-sd06\",dependency=\"$dependency\",result!=\"ok\"}) > 0"
        wait_for 10 prom_result "privchat_dependency_probe_duration_seconds_bucket{job=\"privchat-sd06\",dependency=\"$dependency\",le=\"+Inf\"} > $count_before"
        wait_for 55 event "$dependency" firing '[a-f0-9]+'
        fingerprint=$(awk -v dep="$dependency" '$1==dep && $2=="firing" {print $3; exit}' events.log)
    fi
    kill -CONT "$pid"
    wait_for 25 healthy_pair 1 1
    if [[ "$mode" = full ]]; then
        wait_for 30 event "$dependency" resolved "$fingerprint"
        echo "[PASS] Alertmanager delivered $dependency firing and resolved for the same fingerprint"
    fi
}

exercise redis "$redis_pid" 0 1
exercise object_storage "$storage_pid" 1 0
if [[ "$mode" = full ]]; then
    [[ "$(sort -u events.log | wc -l | tr -d ' ')" = 4 ]]
fi
