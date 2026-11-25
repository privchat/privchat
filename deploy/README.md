# Deploy templates

TLS termination for the gray deployment that runs on a bare IP. The current
self-managed domain cannot be used on this mainland host before ICP filing, so
native clients authenticate a long-lived self-signed certificate by its SPKI.

**One certificate covers everything.** QUIC, TLS/TCP, the platform API and the
file control plane all present the same key, so a client carries a single pin
and one rotation switches all four.

## Certificate

```bash
./scripts/gen-server-tls.sh <host-or-ip> /etc/privchat/tls   # prints the SPKI
```

The private key is `0600 root`. nginx workers run as `nginx` and cannot read
that, so give them a copy rather than loosening the original:

```bash
install -d -m 750 -o root -g nginx /etc/privchat/tls-nginx
install -m 640 -o root -g nginx /etc/privchat/tls/server.crt /etc/privchat/tls-nginx/
install -m 640 -o root -g nginx /etc/privchat/tls/server.key /etc/privchat/tls-nginx/
```

Nothing here belongs in the repository: no certificate, no key, no SPKI value.
The pin goes into the client brand profile, which is a separate decision.

## Ports

nginx takes the public port and the application processes move to private
backend ports. Set `server_host = "127.0.0.1"` so the backend is unreachable
from outside by construction — a security-group rule is an operational
convention, not a guarantee, and one wrong entry exposes a plaintext upload
endpoint.

| Plane | Public (nginx, TLS) | Backend (loopback) | Config |
|---|---|---|---|
| Platform API | 8080 | 8081 | `privchat-application/config/application.conf`: `port = 8081`
| File control | 9083 | 9084 | `privchat-server` `config.toml`: `server_port = 9084`, `server_host = "127.0.0.1"`, `server_api_base_url = "https://<ip>:9083/api/app"`

Copy the matching file from `deploy/nginx/` into `/etc/nginx/conf.d/`, then
`nginx -t && systemctl reload nginx`.

## Order matters

🔴 **Ship the pinned client before enabling a plane.**

The Rust SDK used a plain `reqwest::Client` for the file control plane. That
trusts system roots and cannot verify a self-signed certificate, so enabling
TLS on 9083 first broke every upload in production on 2026-08-31 and had to be
rolled back. The platform API had the same shape and was safe only because its
client had already shipped with pinning.

Before enabling a plane, confirm every client that talks to it pins the SPKI:

- Platform API — `PlatformHttpClient` on iOS and Android
- File control plane — `file_plane_http::control_client` in the Rust SDK
- Object storage (COS presigned URLs) — **must stay on system roots**; pinning
  our own key there rejects every presigned URL

## Browser clients

Browsers cannot install an application-specific SPKI verifier for `fetch`.
Web, H5 and Cocos Web therefore require a browser-trusted HTTPS certificate;
the native self-signed pin is not a browser deployment mechanism. They still
use the same resumable upload protocol and never fall back to public HTTP.

For direct COS uploads, configure bucket CORS to allow the browser origins,
`PUT`, and the `x-amz-checksum-sha256` request header. Expose that header only
if a client needs to read it. Keep the bucket private; clients receive only
short-lived signed part URLs.

## Restarting

`systemctl restart` on the application frees its port asynchronously. nginx
binding the same port immediately after fails with `Address already in use`
even though nothing holds it by the time you look. Wait a few seconds and
start nginx again; it is a release window, not a configuration error.

### Timeline delivery DEAD alert

Load `prometheus/timeline-dispatch.rules.yml` through the monitoring deployment's
`rule_files` and route `PrivChatTimelineDeliveryDead` through the existing
Alertmanager. Scrape the PrivChat `/metrics` listener, not the management liveness
endpoint. The counter has only a bounded cause label:
`delivery_failed` or `lease_expired`. Check structured logs and the committed
timeline before deciding whether client catch-up is sufficient. Do not replay
raw historical envelopes or reset attempts directly in PostgreSQL.

The rule file does not configure Alertmanager or prove that notifications arrive.
Verify firing, routing and resolution in the deployment environment.

### Dependency and dispatch observability

Load `prometheus/dependencies.rules.yml` with the existing Prometheus rules.
`PrivChatDependencyUnavailable` fires after 30s of failed probes and identifies
`redis` or `object_storage` independently. Configure `send_resolved: true` in the
actual receiver integration. Verify that the same alert reaches the normal
on-call destination in both states; an Alertmanager UI status or a successful
test webhook does not establish delivery to that destination. Use a staging
target or a scheduled, clearly labeled canary through the actual routing tree.
Do not suspend production Redis or S3 to test notifications.

The management `/api/service/system/ready` endpoint still tests PostgreSQL
only. It can return 200 while either dependency is degraded. Scrape `/metrics`
internally, and monitor Prometheus `up` for lost scrapes separately; a missing
probe series is not a healthy probe.

| Signal | Interpretation |
| --- | --- |
| `privchat_dependency_up`, `privchat_dependency_probes_total` | Last outcome and counts by dependency and bounded result (`ok`, `error`, `timeout`). Probes run every 15s with a 2s deadline. |
| `privchat_dependency_probe_duration_seconds_bucket` | Redis PING and default S3 list latency. Use `histogram_quantile(0.95, sum by (dependency, le) (rate(privchat_dependency_probe_duration_seconds_bucket[5m])))` for a fleet view. |
| `privchat_timeline_dispatch_pending_events`, `privchat_timeline_dispatch_oldest_pending_seconds` | Durable pending outbox events and age of oldest, not recipient count. Depth caps at 1,000; at that value read it as 1,000+. Check `privchat_timeline_dispatch_backlog_sample_ok == 1` before using either value. Replicas sampling the same database should use max, not sum. |
| `privchat_offline_queue_depth`, `privchat_offline_fallback_total`, `privchat_rpc_requests_total{result="error"}`, `privchat_delivery_failure_sessions_total` | Existing process-local backlog and error indicators; they do not replace the durable outbox gauges. |

The S3 probe uses a read-only one-entry list through the configured default
OpenDAL source and requires list access to that prefix. It is not emitted for
local storage and does not prove write, CDN, resume, or secondary-source
availability. No bucket names, object keys, credentials or request errors are
used as labels.

### Targeted timeline recovery

After the timeline dead-letter alert, locate the exact `event_id` and
`user_id` in the dispatch log or database. Use the authenticated management
listener and send `POST /api/service/messages/dispatch/recover`:

```json
{
  "request_id": "2bb8c763-eecf-414d-8c41-64d093d85c8a",
  "event_id": "9876505",
  "user_id": "9876502"
}
```

Use a new UUID for each deliberate attempt and the same UUID when retrying
an uncertain HTTP response. `notified_sessions` counts online handoffs,
not repaired messages; zero means none, and null means the result was not
recorded. The DEAD row remains for diagnosis. Rust SDKs with timeline-hint
support re-run normal authorized difference sync; other clients still use
the regular catch-up path. This operation does not reconstruct an already
corrupt or incorrectly advanced local cursor.
