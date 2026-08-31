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
backend ports. Do not open the backend ports in the cloud security group or the
host firewall.

| Plane | Public (nginx, TLS) | Backend (loopback) | Config |
|---|---|---|---|
| Platform API | 8080 | 8081 | `privchat-application/config/application.conf`: `port = 8081`
| File control | 9083 | 9084 | `privchat-server` `config.toml`: `server_port = 9084`, `server_api_base_url = "https://<ip>:9083/api/app"`

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
