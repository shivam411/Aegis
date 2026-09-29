# Security

Aegis builds and runs code. Anyone who can call its deploy API can run programs as the daemon's user, so every way in is authenticated. This page covers how, and how to expose the web API safely.

## What is protected, and how

| Entry point | Who can use it | Protection |
| :--- | :--- | :--- |
| gRPC (`127.0.0.1:50051`) | The local CLI | **Loopback only.** The daemon refuses a non-loopback `daemon.host`. Any local account can reach it, as with Docker's socket; on shared machines restrict who can log in. |
| HTTP API (`/api/v1`) | Signed-in users and API tokens | Login required on every route except `POST /api/v1/auth/login`. |
| Webhooks (`/hooks/github/{project}`) | GitHub | HMAC-SHA256 signature with a per-project secret. |
| `GET /healthz` | Anyone who can reach the port | Returns `ok` and nothing else. |
| Admin commands (`aegis-daemon admin …`) | Whoever can read and write the database file | Filesystem permissions. |

### Accounts and sessions

- On first start with the web API enabled, the daemon creates the `admin` account. It writes a random password to `<data_dir>/initial-admin-password` (mode `0600`) and logs the path, not the password. Once you change the password, the file is deleted on the next start.
- Passwords are stored as **Argon2id** hashes. They must be at least 12 characters. The database file is created with mode `0600`.
- Signing in sets the `aegis_session` cookie: random 256-bit, `HttpOnly`, `SameSite=Strict`, `Path=/`, plus `Secure` when clients use HTTPS. The server stores only its SHA-256 hash.
- Sessions end after `web.session_idle_minutes` of inactivity (default 120) or `web.session_max_hours` after sign-in (default 24), whichever comes first. Changing or resetting the password ends every session.
- **CSRF:** requests that change state with a session cookie must send the session's token in `X-CSRF-Token`. The token is returned by login and by `GET /api/v1/auth/session`. Those requests must also be `Content-Type: application/json`, which browsers can't send cross-site without a preflight, and Aegis never grants one.
- **Throttling:** after 5 failed sign-ins from one client address within 15 minutes, that address is locked out for 15 minutes. After 20 failures for one username from any mix of addresses, the username is locked for 15 minutes. Locked attempts get `429` with `Retry-After`. The per-username limit is higher so a single attacker can't lock you out. Each attempt is counted *before* the password is checked, so parallel requests can't slip past the limit. At most 4 password checks run at once, which bounds the memory Argon2 can use. Failed attempts are recorded as `UserLoginFailed` events, never with the password, and with the username only if that account exists.
- Lost the password? On the server run `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin reset-password` (add `--stdin` to choose one).

### API tokens

- Create them in the API (`POST /api/v1/tokens`, signed in) or on the server with `aegis-daemon admin create-token --name ci --scope deploy`. The token is shown once; only its SHA-256 hash is stored.
- **Scopes:** `read` allows only `GET` requests. `deploy` allows everything except managing tokens, passwords and webhook secrets; those always need a signed-in person.
- **Treat a `deploy` token like the admin password.** Deploying means running code as the `aegis` user, and that user can read the daemon's database. The endpoint restrictions stop mistakes, not a determined holder of a deploy token. Give automation `read` tokens where it only needs to observe.
- Use them as `Authorization: Bearer aegis_…`. Revoke them with `DELETE /api/v1/tokens/{id}`.

### Audit log

Every change made over the API is a domain event with an `actor` field: `user:admin`, `token:ci`, `webhook:github`, or `cli` for the local CLI. Sign-ins, sign-outs, failed sign-ins, password changes and token creation/revocation are recorded too. Read them with `GET /api/v1/events` or `aegis timeline`. Passwords, tokens and webhook secrets never appear in events. Open event and log streams re-check their credential every 15 seconds and close once it is revoked or the password changes.

### Request checks

- **Host:** only loopback names or names in `web.allowed_hosts` are accepted. This blocks DNS-rebinding attacks.
- **Origin / Sec-Fetch-Site:** cross-site requests are rejected.
- **Headers on every response:** `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'` on the API (the dashboard's own files get a policy that allows only same-origin scripts, styles, images and connections, with no inline code), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and `Cache-Control: no-store`, plus `Strict-Transport-Security` over HTTPS.

## Exposing the web API

The daemon **refuses to start** with a configuration that would expose the API unsafely:

- a non-loopback `web.host` without `[web.tls]` (passwords would cross the network in clear text);
- public access (`web.behind_proxy = true` or a non-loopback host) without `web.allowed_hosts`;
- half-configured TLS, or a non-loopback `daemon.host` (gRPC).

`aegis-daemon check-config` runs the same checks without starting.

### Option A: SSH tunnel (no public exposure)

Keep the defaults (`127.0.0.1:8420`) and connect through SSH:

```bash
ssh -N -L 8420:127.0.0.1:8420 you@your-server
```

### Option B: reverse proxy with automatic HTTPS (recommended)

Aegis stays on loopback, and Caddy (or nginx with certbot) terminates HTTPS:

```toml
[web]
enabled = true
host = "127.0.0.1"
port = 8420
behind_proxy = true
allowed_hosts = ["aegis.example.com"]
```

```
# /etc/caddy/Caddyfile
aegis.example.com {
    reverse_proxy 127.0.0.1:8420
}
```

With `behind_proxy = true`, cookies are marked `Secure`, responses carry HSTS, and login throttling uses the client address from `X-Forwarded-For`. Aegis trusts that header only from a loopback peer, and uses its **last** entry. The proxy must therefore set or append that header on every request, as Caddy and nginx's `$proxy_add_x_forwarded_for` do. A proxy that passes a client-supplied header through unchanged would let clients choose their throttling address. The per-username limit still applies either way. `install.sh --server --domain aegis.example.com` writes this configuration and prints the Caddy snippet.

Streaming endpoints (`/events/stream`, `/logs/stream`) need the proxy not to buffer responses. Caddy streams them by default; for nginx, set `proxy_buffering off;` for `/api/v1/`.

### Option C: TLS in Aegis

```toml
[web]
enabled = true
host = "0.0.0.0"
port = 8443
allowed_hosts = ["aegis.example.com"]

[web.tls]
cert_path = "/etc/letsencrypt/live/aegis.example.com/fullchain.pem"
key_path  = "/etc/letsencrypt/live/aegis.example.com/privkey.pem"
```

TLS 1.2/1.3 via rustls, with HTTP/2 and HTTP/1.1. Aegis re-reads the files within a minute when they change, so certificate renewals need no restart (a restart would restart every app). A certificate and key that don't match, as can happen mid-renewal, are ignored until they do. The `aegis` user must be able to read the key file. Aegis does not obtain certificates itself: use certbot, or Option B.

## Webhooks

1. Give the project a `repository_url` (webhook deployments clone it).
2. Signed in, call `POST /api/v1/projects/{project}/webhook`. It returns the payload path and a secret, shown once. Calling it again replaces the secret.
3. In GitHub, add a webhook with payload URL `https://aegis.example.com/hooks/github/<project-id>`, content type `application/json`, that secret, and the "push" event.

Only pushes to the project's branch deploy, and always from the project's **configured** `repository_url` at the pushed commit, never a URL from the payload. Deliveries without a valid `X-Hub-Signature-256` get `401`. Redeliveries of the same `X-GitHub-Delivery` are ignored, and so are pushes for commits that already have a release, so a captured signed payload can't be replayed to roll the app back. Repository URLs that git could treat as options or command-running remote helpers (`ext::`) are rejected. Private repositories need credentials the `aegis` user can use, such as an SSH deploy key in `/var/lib/aegis/.ssh` with an `ssh://` URL.

## The service (`install.sh --server`)

- Runs as a dedicated `aegis` system user. Config lives in `/etc/aegis/aegis.toml` (`0640 root:aegis`) and state in `/var/lib/aegis` (`0750`). The database is kept at `0600`.
- The systemd unit applies `NoNewPrivileges`, `ProtectSystem=strict` (writable: `/var/lib/aegis` only), `ProtectHome=read-only`, `PrivateTmp`, `PrivateDevices`, kernel/clock/hostname protections, `RestrictNamespaces`, `RestrictSUIDSGID` and `UMask=0027`. **Your apps inherit these restrictions.**
- `ExecStartPre=aegis-daemon check-config` stops a bad configuration from starting.
- The installer never changes firewall rules. With Option B, open 80/443 for the proxy yourself.

## Known limitations

- A single `admin` account; there are no roles yet (roadmap Phase 6).
- Anyone with a shell on the server who can reach `127.0.0.1:50051` can use the gRPC API. With `behind_proxy`, local processes (including your apps) can also send their own `X-Forwarded-For` to the loopback API port.
- Credentials embedded in a repository URL (`https://user:token@…`) are stored in events that `read` tokens can see; they are redacted only in build logs. Prefer SSH deploy keys.
- Apps run as the same user as the daemon, so a compromised app can read the daemon's database. Per-app users are future work.
- No built-in ACME; use certbot or a reverse proxy.

## The web dashboard

The dashboard's HTML, JavaScript and CSS are compiled into the daemon and served without authentication, because they contain no data. Everything it shows comes from `/api/v1` with the session cookie, and every change it makes carries the CSRF token, so it has exactly the access the signed-in user has. It builds pages from DOM nodes (never `innerHTML` with data), loads nothing from third parties, and runs under a policy that blocks inline scripts. The CSRF token is kept only in memory: the page fetches it from `/auth/session` after a reload.
