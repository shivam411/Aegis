# Migrating from PM2 and systemd

`aegis migrate` reads a PM2 ecosystem file or a systemd service unit and writes an `aegis.toml` for the app. It is experimental: it carries over the start command, the port, the environment and the restart policy, and prints a warning for everything it can't carry over. Review the file, then run `aegis validate` and `aegis deploy`.

The examples use `aegis`, a link to the `aegis-cli` binary that the installer creates (see [Installation](install.md)); `aegis-cli` works the same.

All keys below are described in [Configuration](configuration.md).

---

## 1. From PM2

Run it in the app's directory (where `aegis.toml` should go):

```bash
aegis migrate pm2                          # reads ./ecosystem.config.js, .cjs, .json or ./ecosystem.json
aegis migrate pm2 --file pm2.json --app api   # a given file; --app picks one of several apps
aegis migrate pm2 --dry-run                # print the result instead of writing aegis.toml
aegis migrate pm2 --force                  # overwrite an existing aegis.toml (its project id is kept)
```

JSON files are read exactly. JavaScript files are read on a best-effort basis: plain object and array literals work, while anything computed (`process.env.X`, function calls, spreads) is skipped with a warning. If your file is mostly code, export it as JSON first (`pm2 ecosystem`, or write `{"apps": [...]}` by hand).

What is carried over:

| PM2 field | `aegis.toml` | Notes |
| :--- | :--- | :--- |
| `name` | `project.name` | A new `project.id` is generated (kept when you use `--force` on an existing file). |
| `script`, `args`, `interpreter`, `interpreter_args` / `node_args` | `build.start_command` | Joined into one shell command. Without `interpreter`, it is picked from the script's extension (`.js` → `node`, `.py` → `python3`, `.sh` → `bash`, ...); `interpreter: "none"` runs the script directly. |
| `env` | `[env]` | Strings, numbers and booleans. A numeric `PORT` becomes `deploy.port` instead (Aegis exports `deploy.port` as `PORT`). |
| `autorestart: false` | `deploy.restart_policy = "never"` | Otherwise the default, `"always"`. |

Not carried over (each prints a warning): `cwd` (Aegis runs the app from its release directory, so put `aegis.toml` in that directory and deploy from there), `env_production` and other `env_*` blocks (copy what you need into `[env]`, the dashboard's Variables or Secrets), `instances` and `exec_mode: "cluster"` (see `run.instances`, which needs a domain on the built-in proxy), `watch` (redeploy instead), `max_memory_restart` (set a memory limit under Resources), `cron_restart` (use `aegis schedule` for timed deployments) and the log file settings (Aegis keeps the logs: `aegis logs`).

The build commands are not taken from PM2 (it has none). Unless you set `build.install_command` and `build.build_command`, Aegis detects them from the project (for Node: `npm ci` with a lockfile, else `npm install`, then `npm run build --if-present`).

## 2. From systemd

```bash
aegis migrate systemd --service /etc/systemd/system/my-app.service
```

`--dry-run` and `--force` work as for PM2. The project name is the unit's file name without `.service` (and without a trailing `@` for template units).

| Unit directive (`[Service]`) | `aegis.toml` | Notes |
| :--- | :--- | :--- |
| `ExecStart=` | `build.start_command` | The first one; with several, the others are ignored with a warning. The `-`, `@`, `:`, `+` and `!` prefixes are dropped (with `@`, the argv[0] word too). `%` specifiers are kept but flagged: only systemd expands them. |
| `Environment=` | `[env]` | Quoted assignments are split like systemd does. A numeric `PORT` becomes `deploy.port`. |
| `EnvironmentFile=` | `[env]` | The file is read while migrating (as systemd does, it overrides `Environment=`); a missing file is a warning unless the path starts with `-`. Move real secrets to the dashboard's Secrets afterwards rather than leaving them in `aegis.toml`. |
| `Restart=` | `deploy.restart_policy` | `always` → `always`; `on-failure` → `on-failure`; `no` → `never`; `on-abnormal`, `on-abort`, `on-watchdog` → `on-failure`; `on-success` → `never` (with a warning). Without `Restart=`, the Aegis default `always` applies. |
| `WorkingDirectory=` | none | Aegis runs the app from its release directory; put `aegis.toml` in that directory and deploy from there. |
| `User=`, `Group=` | none | Apps run as the daemon's user. |
| `ExecStartPre=`, `ExecStartPost=`, `ExecReload=`, `ExecStop=` | none | Use `build.install_command` / `build.build_command` for setup steps. Aegis stops apps with SIGTERM, then SIGKILL after `deploy.drain_timeout_secs`. |

Other directives (resource limits, sandboxing) are ignored. Limits can be set per app in the dashboard (Resources); the daemon's own systemd unit applies its hardening to every app.

## 3. After migrating

- **Secrets don't belong in `aegis.toml`.** It is copied into every release. Move passwords, keys and connection strings out of `[env]` into the dashboard's Settings → Secrets (multi-line values such as PEM keys can be pasted as they are).
- **Secrets are not available at build time.** Only the running app gets them. Anything the build reads must be a plain variable (`[env]` or the dashboard's Environment variables), for example Next.js `NEXT_PUBLIC_*` values, which are inlined into the bundle by `next build`.
- **Files your app reads that git ignores are not copied.** A deployment copies tracked files plus untracked files that aren't ignored. A `.env`, `.env.local` or `.env.production` that is in `.gitignore` is not part of the release. Point the app's **Env file** at the existing file on the server (section 4.4), or import it into Variables and Secrets.
- **Leave `NODE_ENV` unset for the build** unless you know the build works with it. With `NODE_ENV=production`, `npm ci`/`npm install` skip `devDependencies`, and a typical Next.js or TypeScript build needs them (`typescript`, `@types/*`, `eslint`, Tailwind/PostCSS plugins), so it fails with "Cannot find module" or "It looks like you're trying to use TypeScript but do not have the required package(s) installed". `next build` and `next start` set `NODE_ENV=production` themselves. Every source in the [precedence table](configuration.md#env-file-on-the-server) except Secrets reaches the build, so if the PM2 `.env`, or the `[env]` that `aegis migrate` copied from the ecosystem file, sets `NODE_ENV=production`, remove that line (or make the install command `npm ci --include=dev`). If the app needs it at runtime only, set it in the start command: `NODE_ENV=production npm start`.
- **Listen on `$PORT`.** Once the app has a domain on the built-in proxy, every release gets its own port in `PORT`, and `deploy.port` is only the default for apps without a domain. A start command with a fixed port (`next start -p 3000`, `--port 8080`) must use `$PORT` instead (`next start -p $PORT`). Check `package.json` scripts too.
- **Apps without a domain have a short gap on every deployment**: the old process is stopped before the new one starts on the same port. Map a domain on the built-in proxy to get zero-downtime switches.

---

## 4. Example: a PM2 app behind nginx

A common setup: nginx terminates TLS on ports 80/443 and forwards `app.example.com` to a Next.js app that PM2 runs on port 3000. The plan is to keep nginx in front, run the app under Aegis behind Aegis's own proxy on a loopback port, and switch nginx over once the Aegis copy works. PM2 keeps serving until then.

### 4.1 Configure the Aegis proxy behind nginx

In the daemon's configuration (`/etc/aegis/aegis.toml` for a `--server` install):

```toml
[proxy]
enabled = true
host = "127.0.0.1"                       # only nginx (and local processes) can reach it
http_port = 8081
https_port = 8444                        # required and must be free; unused behind nginx
trusted_proxies = ["127.0.0.1", "::1"]   # keep nginx's X-Forwarded-Proto/-Host
# response_timeout_secs = 120            # raise it for slow endpoints (also raise nginx's proxy_read_timeout)
```

Leave `[proxy.acme]` off: nginx has the certificates. Then `sudo systemctl restart aegis` (`aegis-daemon check-config` runs first and names any mistake).

Without `trusted_proxies`, Aegis overwrites `X-Forwarded-Proto` with `http` (it only sees nginx's plain-HTTP connection), which breaks apps that build absolute URLs or decide on Secure cookies from it. See [Security](security.md#forwarded-headers-and-trusted-proxies) for what trusting a proxy means.

### 4.2 Create the app

In the app's directory on the server (owned by a user the daemon can read; with a `--server` install, the `aegis` user):

```bash
aegis migrate pm2 --app my-app     # writes aegis.toml
aegis init --name my-app           # registers the app with the daemon; keeps the aegis.toml above
aegis validate
```

`aegis init` only registers here (it doesn't deploy), and it keeps an `aegis.toml` that already has a `[project]` section.

Check the generated `aegis.toml`:

- `build.start_command` must listen on `$PORT`. For Next.js: `start_command = "npx next start -p $PORT"` (or `npm start` with `"start": "next start -p $PORT"` in `package.json`; `next start` also reads `PORT` when `-p` is not given).
- Move secrets from `[env]` to the dashboard (below).

### 4.3 Map the domain before the first deployment

With the app registered (previous step), in the dashboard open the app's **Settings** tab and, under **Domains**, add `app.example.com` with TLS **HTTP only**. (The API equivalent is `POST /api/v1/projects/my-app/domains` with `{"domain": "app.example.com", "tls": "off"}`, from a signed-in session; API tokens can't change domains.)

Do this **before** the first `aegis deploy`: with a domain, each release gets a free port of its own, so the Aegis copy can start while PM2 still holds port 3000. Without a domain it would try to use `deploy.port` (3000) and fail its health check.

Use TLS `off` because nginx terminates HTTPS. (With `acme` or `files`, requests nginx marks as `X-Forwarded-Proto: https` are served without a redirect, so there is no loop, but the certificate would go unused.)

### 4.4 Variables and secrets

The app's settings are in the `.env` PM2 used, say `/opt/github/my-app/.env`. The simplest move is to keep using that file:

1. Allow its directory in the daemon configuration (`/etc/aegis/aegis.toml`) and restart Aegis:

   ```toml
   [apps]
   env_file_dirs = ["/opt/github"]
   ```

   ```bash
   sudo systemctl restart aegis
   ```

2. Let the `aegis` user read it (it runs under `ProtectSystem=strict`, so it can read `/opt` but not write it, and the unit doesn't need to change):

   ```bash
   sudo chgrp aegis /opt/github/my-app/.env
   sudo chmod 640 /opt/github/my-app/.env
   namei -l /opt/github/my-app/.env    # every directory above needs x for aegis (group or other)
   ```

3. In **Settings → Environment**, set **Env file path** to `/opt/github/my-app/.env` and save. The card shows whether Aegis can read it and every variable name it provides (never the values). If it says *permission denied*, repeat step 2 for the file or the directory it names.

The file is read at every build (so `NEXT_PUBLIC_*` values reach `next build`) and every start. To change a value, edit the file and restart the app (or deploy, for build-time values). To override one value without editing the file, add a **Variable** (also seen by the build) or a **Secret** (start only) of the same name: the precedence is env file < `[env]` in `aegis.toml` < Variables < Secrets, and `PORT` is always set by Aegis (a `PORT` in the file is ignored). See [configuration](configuration.md#env-file-on-the-server).

Remove `NODE_ENV=production` from the file unless the build works with it (see section 3): `next build` and `next start` set it themselves.

If you'd rather not keep the file, use **Settings → Environment → Import .env** to paste it (or pick it; it's read in the browser) and store each name as a Variable or a Secret. The import suggests a type per name: Secret for names containing `KEY`, `SECRET`, `TOKEN`, `PASSWORD`, `PRIVATE` or `AUTH`, URLs with a password and multi-line values, Variable for `NEXT_PUBLIC_*` and the rest. Remember:

- **Variables**: everything the build needs, including all `NEXT_PUBLIC_*` values, plus non-secret settings.
- **Secrets**: passwords, API keys, `DATABASE_URL`, private keys. They reach the running app, not the build.

### 4.5 Deploy and test through the Aegis proxy

```bash
aegis deploy
curl -sI -H "Host: app.example.com" -H "X-Forwarded-Proto: https" http://127.0.0.1:8081/
aegis logs -f
```

The `curl` goes to Aegis's proxy exactly as nginx will. Fix anything here while PM2 is still serving the real traffic.

### 4.6 Switch nginx

In the site's `server` block for `app.example.com`, point the upstream at Aegis's proxy instead of the app:

```nginx
location / {
    proxy_pass http://127.0.0.1:8081;          # was http://127.0.0.1:3000
    proxy_set_header Host $host;               # Aegis routes by Host
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-Host $host;
    proxy_http_version 1.1;                    # WebSockets / HMR
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
}
```

(`$connection_upgrade` needs `map $http_upgrade $connection_upgrade { default upgrade; '' close; }` in the `http` block; many configurations already have it.)

```bash
sudo nginx -t && sudo systemctl reload nginx
```

Check the site in a browser. Then stop the PM2 copy and make sure it doesn't come back at boot:

```bash
pm2 stop my-app
pm2 save
```

From now on, deploy with `aegis deploy` (or a webhook or schedule); the switch to each new release happens in Aegis's proxy without dropped requests.

### 4.7 The way back

If something is wrong after the switch:

1. `pm2 start my-app` (or `pm2 resurrect`), and wait until it answers on port 3000.
2. Set `proxy_pass` back to `http://127.0.0.1:3000;`, then `sudo nginx -t && sudo systemctl reload nginx`.
3. `aegis stop my-app`, so the two copies don't both run jobs or migrations. `pm2 save` again once PM2 is serving.

Nothing in nginx's TLS setup changed, so going back is just these steps. The Aegis app, its releases and secrets stay in place for another attempt.
