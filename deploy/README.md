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

#### Targeted timeline recovery

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
