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
- Lost the password? On the server run `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin reset-password` (add `--user NAME` for another account, `--stdin` to choose one).

### Users and roles

Each person signs in with their own account, so the audit log records who did what (`actor` = `user:<name>`). Every account has one role; each includes the ones before it:

| Role | Can |
| :--- | :--- |
| **viewer** | Read everything except credentials: apps, releases, deployments, logs, events, metrics, domains. View the `aegis.toml` an app would get. Change their own password. |
| **deployer** | Also deploy, roll back, and start, stop or restart apps. |
| **admin** | Everything: create apps, change settings, variables, secrets, domains, schedules, webhooks and limits; manage deploy keys, scan server folders and import apps; manage API tokens, users and backups. |

- Admins manage users in the dashboard (**Account → Users**) or with `GET/POST /api/v1/users`, `PUT/DELETE /api/v1/users/{name}`. New users and admin password resets get a random password that is shown once.
- A role change applies to the user's open sessions immediately. A password reset or deletion signs them out.
- There is always at least one admin: the last admin can't be demoted or deleted, and nobody can delete their own account.
- On the server: `aegis-daemon admin create-user --name ana --role deployer`, `admin set-role --name ana --role admin` (e.g. to regain an admin) and `admin list-users`.
- Roles are enforced by the server on every request; the dashboard only hides controls a role can't use.
- Accounts that existed before roles were added (the original `admin`) are admins.

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

## Forwarded headers and trusted proxies

This is about the built-in app proxy (`[proxy]`), not the web API's `behind_proxy` above.

By default the app proxy trusts no one: it **replaces** `X-Forwarded-Proto` and `X-Forwarded-Host` on every request with what it saw itself (the listener the request arrived on, and the requested host), **appends** the connecting peer's address to `X-Forwarded-For`, and **removes** `X-Real-IP` and `Forwarded` (RFC 7239). A client can put anything in the earlier `X-Forwarded-For` entries, but never remove or replace the last one, so apps should only believe the entries added by proxies they trust (the last one, when the Aegis proxy is the only proxy).

When Aegis runs behind another reverse proxy that terminates TLS (nginx, Caddy), every request arrives from that proxy over plain HTTP, so apps would always see `http`. `[proxy] trusted_proxies` fixes that: for a peer in the list, the incoming `X-Forwarded-Proto` and `X-Forwarded-Host` are passed through (when well formed: `http`/`https` and a plain host[:port], sent once), and `X-Forwarded-Proto: https` also stops the proxy from redirecting the request to HTTPS again.

What this trusts, and the risks:

- **Anyone who can connect from a trusted address can set these headers.** An app may then believe a plain-HTTP request was HTTPS (and send Secure cookies over it, or skip its own redirect), or build absolute URLs (password-reset links, OAuth redirects) for a host of the caller's choosing. So list only the front proxy's own addresses, and make sure the front proxy **sets** both headers on every request rather than passing a client's values through (nginx: `proxy_set_header X-Forwarded-Proto $scheme; proxy_set_header X-Forwarded-Host $host;`).
- **Loopback is the safe case.** With `[proxy] host = "127.0.0.1"` and `trusted_proxies = ["127.0.0.1", "::1"]`, only processes on the server can reach the proxy at all. Those include your apps, which could already reach each other's ports directly.
- **Other addresses** (a load balancer on a private network) are accepted but logged as a warning at startup. Use the narrowest range possible, and firewall the proxy ports so only that host can reach them. Host names are refused (they could change under you), and so are `0.0.0.0/0` and `::/0`.
- From a trusted peer, `X-Forwarded-For`, `X-Real-IP` and `Forwarded` are passed through **unchanged** (the trusted proxy's own address is not appended; `X-Forwarded-For` is set to the peer only when the front proxy sent none). The app sees exactly what the front proxy sent, so its "trust N proxy hops" setting stays what it was without Aegis. This is safe only because the front proxy itself appends its client (nginx `$proxy_add_x_forwarded_for`, Caddy by default): configure it so, or clients can choose the address the app sees. Aegis itself does not use these headers for any decision except the HTTPS redirect.
- ACME HTTP-01 challenges are answered on the plain listener whatever the headers say; they only reveal the public key authorization the CA asks for.

## App environment: variables, secrets and env files

- **Variables** are plain settings: every role can read them, they are written into each release's `aegis.toml`, recorded in `ProjectSettingsUpdated` events, and passed to build commands.
- **Secrets** are encrypted at rest (ChaCha20-Poly1305, key in `<data_dir>/secrets.key`), write-only over the API, managed by signed-in admins only (never API tokens), passed only to the app's process when it starts, and never written into a release, an event, a log or a build's environment.
- **Env files on the server** (`[apps] env_file_dirs` + Settings → Environment) are read by the daemon at every build and process start and never stored by Aegis: not in the database, events, releases, the build log (names only) or API responses (names only, for every role). The trust model:
  - *Who chooses the file:* only an admin (or a `deploy` API token, which can already change build commands and so run code as the daemon's user), through the app's settings. The repository's `aegis.toml` can't name one.
  - *Which files:* only regular files of at most 1 MiB inside the directories the server's administrator listed in `[apps] env_file_dirs` (empty by default: off), after resolving symbolic links, so a link or `..` can't point outside them. Anything the daemon's user can read inside those directories can be chosen by any admin, so list directories that hold nothing but env files for this server's apps, not `/` or `/etc`.
  - *Who can read the file:* whoever the file's permissions allow, plus the daemon's user (`aegis`), which needs read access (`chgrp aegis FILE && chmod 640 FILE`). The values are then visible to the app (which runs as the same user) and to build commands, whose output is in the build log that every role can read. Don't keep values in it that build scripts print. Aegis itself never reads a value back out to anyone.
  - *Errors* (missing file, permission denied, outside the allowed directories, syntax) name the path and the line number, never a value.
  - The daemon's unit is not loosened for this: `ProtectSystem=strict` keeps `/opt` and `/etc` read-only and `ProtectHome=read-only` keeps `/home` readable but not writable.
- **Import .env** (dashboard) sends the pasted text (or a file read in the browser; it is not uploaded as a file) to the server, which parses it and returns only names and suggested types; the import itself parses it again and stores each value as a Variable or a Secret. It needs a signed-in admin (not an API token), and the text is never logged or stored as such.

## Webhooks

1. Give the project a `repository_url` (webhook deployments clone it).
2. Signed in, call `POST /api/v1/projects/{project}/webhook`. It returns the payload path and a secret, shown once. Calling it again replaces the secret.
3. In GitHub, add a webhook with payload URL `https://aegis.example.com/hooks/github/<project-id>`, content type `application/json`, that secret, and the "push" event.

Only pushes to the project's branch deploy, and always from the project's **configured** `repository_url` at the pushed commit, never a URL from the payload. Deliveries without a valid `X-Hub-Signature-256` get `401`. Redeliveries of the same `X-GitHub-Delivery` are ignored, and so are pushes for commits that already have a release, so a captured signed payload can't be replayed to roll the app back. Repository URLs that git could treat as options or command-running remote helpers (`ext::`) are rejected. Private repositories are cloned with the app's [deploy key](#deploy-keys), for webhook and scheduled deployments too.

## Deploy keys

A private repository is cloned with a **per-app SSH deploy key** (dashboard: Settings → Repository, or the new app wizard when Aegis can't read a repository; API: `POST/GET/DELETE /api/v1/projects/{p}/deploy-key`, `POST …/deploy-key/test`). The trust model:

- **One key per app, read-only.** Aegis generates an Ed25519 key pair with the RustCrypto `ssh-key` crate (no `ssh-keygen`, no shell). You add the public key to that one repository's deploy keys with *Allow write access* **unticked**, so the key can read one repository and nothing else. Replacing or deleting the key in Aegis doesn't remove it from GitHub; do that there too.
- **The private key never leaves the daemon.** It is sealed like secrets (ChaCha20-Poly1305 with `<data_dir>/secrets.key`, bound to the project so a sealed key can't be moved to another app) in the database's `deploy_keys` table. The API returns only the public key, its `SHA256:` fingerprint and the creation time, for signed-in admins only (API tokens and other roles get `403`, even to read). Events (`DeployKeyCreated`, `DeployKeyDeleted`) carry the fingerprint, never key material; releases, build logs and the dashboard never contain it.
- **On disk only while git runs.** For each clone or access test the key is decrypted and written to a 0600 file in a fresh 0700 directory `<data_dir>/run/ssh/key-<random>/`, and git runs with `GIT_SSH_COMMAND="ssh -i <that file> -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=<aegis-managed known_hosts>"`: only paths appear on the command line and in the environment, never key material. The directory is deleted when the command ends, whether it succeeded or failed, and leftovers from a crash are deleted at startup. While it exists, the app's own processes (which run as the same `aegis` user) could read it; they can already read the database and `secrets.key`, so this adds nothing they couldn't get.
- **Host keys are pinned, never trusted on first use.** The `known_hosts` file of each git command holds GitHub's published host keys (built into the binary; [GitHub's SSH key fingerprints](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints)) plus the contents of `[git] known_hosts_file`, if set. Any other host fails with `Host key verification failed`, and *Test access* explains how to add it.
- **Which URLs use it.** SSH remotes (`ssh://…`, `git@host:owner/repo.git`). A GitHub HTTPS URL is cloned over SSH (`git@github.com:owner/repo.git`) when the app has a key; other HTTPS URLs and local paths never use it. The existing URL hardening (no `ext::`/remote helpers, no leading `-`, no whitespace) still applies.
- **Backups** contain the database (with the sealed keys) and `secrets.key`, so a backup can decrypt every deploy key (as it can every secret): keep downloads safe. Losing `secrets.key` makes the keys unreadable; generate new ones.
- **Access checks.** *Test access* runs `git ls-remote` with the key (20 s timeout) and shows git's own error output (which never contains the key). The wizard's and the scan's visibility check (`POST /api/v1/apps/check-access`) runs an unauthenticated `git ls-remote https://github.com/<owner>/<repo>.git` (no credential helpers, no prompts) and only accepts GitHub repository URLs, so it can't be pointed at other hosts.

## Scanning folders on the server

*Scan a folder on the server* (`POST /api/v1/apps/scan`) and *Import selected* (`POST /api/v1/apps/import`) are for signed-in admins only. They only look inside the folders listed in `[apps] import_dirs` or `[apps] env_file_dirs` (empty: off), after resolving symbolic links; a link leading outside them is not followed. A scan goes two levels deep and looks at no more than 5000 entries. It never runs git or any other program, and never writes. Per app folder it reads, read-only:

- the files the runtime detector looks at (`package.json`, `aegis.toml`, `Cargo.toml`, `pyproject.toml`, `requirements.txt`, `go.mod`, `pom.xml`, `build.gradle`, `Dockerfile`);
- `.git/HEAD` and `.git/config` (parsed directly, so root-owned checkouts don't need `safe.directory`), for the `origin` URL and the branch. Credentials embedded in the URL (`https://token@github.com/…`) are removed and never returned or imported;
- the app's `.env`, for **variable names only**; values are never returned, logged or stored. An unreadable `.env` is reported with the `chgrp`/`chmod` fix;
- whether something listens on the app's port (a local bind and connect test, plus `/proc/net/tcp*` and `/proc/<pid>/fd` to name the process when the daemon's user can see it).

Importing registers the selected apps (`ProjectCreated`, `ProjectSettingsUpdated`, and one `AppsImported` with their ids and names) without deploying them.

## The service (`install.sh --server`)

- Runs as a dedicated `aegis` system user. Config lives in `/etc/aegis/aegis.toml` (`0640 root:aegis`) and state in `/var/lib/aegis` (`0750`). The database is kept at `0600`.
- The systemd unit applies `NoNewPrivileges`, `ProtectSystem=strict` (writable: `/var/lib/aegis` only), `ProtectHome=read-only`, `PrivateTmp`, `PrivateDevices`, kernel/clock/hostname protections, `RestrictNamespaces`, `RestrictSUIDSGID` and `UMask=0027`. **Your apps inherit these restrictions.**
- `ExecStartPre=aegis-daemon check-config` stops a bad configuration from starting.
- `Delegate=yes` gives the daemon its own cgroup subtree for per-app limits. `/sys/fs/cgroup` is left writable for that, and systemd's file ownership still confines the `aegis` user to its own subtree. The daemon never needs root for limits under this unit. Run by hand as root, it creates `/sys/fs/cgroup/aegis` instead.
- The installer never changes firewall rules. With Option B, open 80/443 for the proxy yourself.
- Unless run with `--no-self-update`, it also installs `aegis-update.path` and `aegis-update.service` (root) for upgrades from the dashboard; see below. The installer itself checks the release's signature (`SHA256SUMS.minisig`, see below) and the downloaded archive against `SHA256SUMS`.

## Self-update

See [`upgrading.md`](upgrading.md) for the procedure.

- **What is verified.** First the release's `SHA256SUMS` must carry a valid signature, `SHA256SUMS.minisig`, by one of the release keys compiled into the running Aegis (below), made for that release (its signed trusted comment names the tag, so a signature can't be replayed on another release). Then the release archive must match the SHA-256 listed for it there, checked before anything is unpacked; only regular files named `aegis-daemon`, `aegis-cli` and `aegis-tui` at the archive's top level are used. The new daemon must then run and report the expected version (`--version`) before it replaces the old one. Downloads must be `https://github.com/<repo>/releases/download/...` URLs (redirects to GitHub's download hosts are followed, HTTPS only), with size limits and timeouts. This applies to "Upgrade now", the root helper, `aegis-daemon self-update` and `aegis upgrade` (all the same code), and `install.sh` checks the same signature with OpenSSL (or minisign) before installing anything.
- **The trust root is the maintainers' release signing key.** The signature scheme is [minisign](https://jedisct1.github.io/minisign/): Ed25519 over the BLAKE2b-512 hash of `SHA256SUMS`, verified offline with the small, dependency-free [`minisign-verify`](https://crates.io/crates/minisign-verify) crate (also used by Tauri's updater). The accepted public keys are in [`core/update/keys/release.pub`](../core/update/keys/release.pub), compiled into the binaries, and in `install.sh`'s `RELEASE_KEYS`; the secret key exists only offline with the maintainers and as a GitHub Actions secret used by the release job (see [`releasing.md`](releasing.md)). So publishing a release is no longer enough to ship an update: someone who controls the GitHub account, a token or the release workflow, but not the key, can't produce a release any Aegis install accepts. TLS to GitHub (certificates checked against the Mozilla root store compiled into Aegis) now only protects metadata and availability. Several keys can be listed for rotation; the procedure is in [`releasing.md`](releasing.md#rotating-the-key).
- **If the signing key leaks**, the attacker can sign releases that existing installs accept, as long as they can also publish them (in the repository, a configured fork, or by breaking TLS to GitHub). There is no revocation list: the maintainers ship a release without the leaked key and announce it, and administrators upgrade by hand ([`releasing.md`](releasing.md#if-the-secret-key-leaks)).
- **Builds without a key.** While `release.pub` holds its placeholder (no key has been generated yet), verification fails closed: the dashboard doesn't offer "Upgrade now" and explains why, the helper refuses, and `self-update`, `aegis upgrade` and `install.sh` refuse unless run with `--allow-unsigned`. That flag exists only on the command line (never in the API or for the helper): it prints a warning, still checks the SHA-256, still rejects a signature that is present but invalid, and the install is recorded in the event log (`UpdateApplied` with `allow_unsigned: true`, recorded when the daemon next starts). The release workflow refuses to publish without the key, so official releases are always signed.
- **What remains trusted on `main`.** New installs (`curl … install.sh`) and new builds trust the keys committed to the repository, so someone who can push to `main` can change them for *future* installs; existing installs keep the keys they were built with.
- **Who can trigger it.** Viewers and deployers can only see that an update exists. Checking now and upgrading need a signed-in admin (with CSRF token); API tokens can't do either. Only the version of the latest stable release (draft and pre-releases excluded) that is newer than the running one can be installed from the dashboard; never a downgrade, never a new major version.
- **Least privilege on servers.** The daemon (user `aegis`, `ProtectSystem=strict`) can't write `/usr/local/bin`. It writes `/var/lib/aegis/update/request.json` containing only a version string. The root helper reads nothing else from the daemon: it rejects symlinks, oversized or malformed requests, deletes the request first, downloads and verifies the release itself, installs only a version newer than its own, may write only `/usr/local/bin` and that directory (`ProtectSystem=strict`, `ReadWritePaths=`), and writes its result files (`result.json`, `installed.json`) fresh (never through an existing file or symlink). It always requires a valid signature (`--allow-unsigned` is refused with `--from-request`). So a compromised `aegis` user can at most make root install a newer release signed with the release key. Install with `--no-self-update` to remove even that; the dashboard then shows the command for an admin to run.
- **Backups before an update.** "Upgrade now" and `aegis-daemon self-update` both take a backup (database, secrets key, certificates; no releases) before the binaries are replaced; the command line takes it once the new release has been downloaded and verified, and nothing is installed if it fails (`--no-backup` skips it). Run as root, `self-update` gives the new archive to the data directory's owner so the daemon can list and rotate it. `--from-request` (the root helper) takes none, because the daemon took it before asking.
- **Privacy.** With `check = true` the daemon contacts `api.github.com` every 12 hours (sending its version in the `User-Agent`, and your server's IP address). Set `[update] check = false` to stop that.

## Known limitations

- A single `admin` account; there are no roles yet (roadmap Phase 6).
- Anyone with a shell on the server who can reach `127.0.0.1:50051` can use the gRPC API. With `behind_proxy`, local processes (including your apps) can also send their own `X-Forwarded-For` to the loopback API port.
- Credentials embedded in a repository URL (`https://user:token@…`) are stored in events that `read` tokens can see; they are redacted only in build logs. Prefer SSH deploy keys.
- Apps run as the same user as the daemon, so a compromised app can read the daemon's database. Per-app users are future work.
- No built-in ACME for the web API's own TLS (`[web.tls]`); use certbot or a reverse proxy. The app proxy (`[proxy.acme]`) does have it.
- With `[proxy] trusted_proxies` set, local processes (including your apps) can send their own `X-Forwarded-Proto`/`X-Forwarded-Host`/`X-Forwarded-For`/`X-Real-IP`/`Forwarded` to the app proxy's loopback port; see [Forwarded headers and trusted proxies](#forwarded-headers-and-trusted-proxies).
- Resource limits constrain CPU, memory and process count, not disk or network use. An app that forks and calls `setsid` still stays in its cgroup (so its limits apply), but outside cgroups (limits unavailable) such children aren't counted in the app's metrics.

## The web dashboard

The dashboard's HTML, JavaScript and CSS are compiled into the daemon and served without authentication, because they contain no data. Everything it shows comes from `/api/v1` with the session cookie, and every change it makes carries the CSRF token, so it has exactly the access the signed-in user has. It builds pages from DOM nodes (never `innerHTML` with data), loads nothing from third parties, and runs under a policy that blocks inline scripts. The CSRF token is kept only in memory: the page fetches it from `/auth/session` after a reload.
