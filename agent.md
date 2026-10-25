# Aegis for AI agents

Prompts and runbooks that let an AI coding agent (Claude Code, Codex, Cursor, a shell-capable chat agent) **install Aegis, migrate apps onto it, and manage access**, without a human reading the docs first.

How to use this file:

1. Open your agent in a shell on (or with SSH access to) the target server.
2. Give it the **[Ground rules](#ground-rules)** once, then paste one **prompt** below, filling the `<placeholders>`.
3. Each prompt ends with *Done when* checks. Ask the agent to show the output of those checks before it reports success.

The runbooks below are written for the agent to follow; humans can follow them too. Authoritative docs: [install](docs/install.md), [migration](docs/migration_guide.md), [configuration](docs/configuration.md), [HTTP API](docs/http_api.md), [security](docs/security.md), [dashboard guide](docs/dashboard/README.md).

---

## Ground rules

Paste this at the start of the session (or into your agent's rules file, e.g. `CLAUDE.md` / `AGENTS.md`):

```text
You are operating Aegis, a single-binary deployment daemon (https://github.com/shivam411/Aegis).
Rules:
- Read docs/install.md, docs/migration_guide.md and docs/security.md of the repository before acting; prefer them over your memory.
- Never print, log, commit or paste secrets: the initial admin password, API tokens, hide passwords, deploy keys, .env contents. Show variable NAMES only. When a secret must reach the user, tell them the file or command that shows it.
- Never run destructive commands (delete an app, restore a backup, pm2 delete, rm -rf, firewall or nginx changes) without asking first, and say what will change.
- Change one thing at a time and verify it (command + expected output) before the next step.
- Keep the old setup (PM2, systemd unit, nginx upstream) running until the Aegis copy is verified. State the way back before each cut-over.
- Prefer `aegis` CLI and the HTTP API over editing Aegis files or its SQLite database by hand.
- If a step fails, stop, show the error and the diagnosis; do not retry blindly or work around a security check (signature, TLS, permissions).
```

Names used below: CLI `aegis` (a link to `aegis-cli`), daemon `aegis-daemon`. A **server install** (`--server`) keeps the config in `/etc/aegis/aegis.toml` and the data in `/var/lib/aegis`, and runs as user `aegis`. The web API and dashboard listen on `127.0.0.1:8420`.

---

## 1. Install Aegis

### Prompt: install on a Linux server (systemd)

```text
Install Aegis on this server as a system service and verify it.

Context: <distro/version, e.g. Ubuntu 24.04>, root or sudo available: <yes/no>.
Public hostname for the dashboard (or "none", then I will use an SSH tunnel): <aegis.example.com | none>.

Steps:
1. Check prerequisites: Linux with systemd (`systemctl --version`), `curl`, `tar`, `openssl` (1.1.1+) or `minisign`, and outbound HTTPS to github.com. Report anything missing; install it with the distro package manager only after telling me.
2. Run the server installer. Without a hostname:
     curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | sudo bash -s -- --server
   With a hostname (prepares the API for a reverse proxy):
     curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | sudo bash -s -- --server --domain <hostname>
   The installer verifies the release signature and checksum. Do NOT pass --allow-unsigned.
3. Verify: `aegis --version`, `systemctl is-active aegis`, `curl -fsS http://127.0.0.1:8420/healthz` (expect `ok`), and `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon check-config`.
4. The first admin password is in `/var/lib/aegis/initial-admin-password` (mode 0600). Do not print it. Tell me to read it myself with `sudo cat /var/lib/aegis/initial-admin-password`, sign in as `admin`, and change it under Account → Password.
5. If I gave a hostname: configure a reverse proxy to 127.0.0.1:8420 (Caddy example: `<hostname> { reverse_proxy 127.0.0.1:8420 }`). Ask before editing proxy or firewall config. Otherwise tell me the tunnel command: `ssh -N -L 8420:127.0.0.1:8420 <user>@<server>`.

Done when: the four checks in step 3 pass, and you have told me where the admin password is without revealing it.
```

### Prompt: install for one user (no root, no service)

```text
Install Aegis for the current user and run the daemon in the foreground for a quick trial.
1. `curl -fsSL https://raw.githubusercontent.com/shivam411/Aegis/main/install.sh | bash` (installs to ~/.local/bin; make sure it is on PATH).
2. `aegis --version`, then start `aegis-daemon` in a second terminal or in the background (`nohup aegis-daemon > ~/aegis.log 2>&1 &`).
3. `aegis status` must answer. Show me the output.
Note: without `[web] enabled = true` there is no dashboard; this mode is for trying the CLI. Use the server install for real use.
```

### Prompt: upgrade Aegis

```text
Upgrade Aegis to the latest release. First `aegis upgrade --check` and tell me the versions. If a newer one exists and I agree: take a backup (`sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin backup`), then `sudo aegis-daemon self-update` (it verifies the signature and restarts the service). Afterwards check `aegis --version`, `systemctl is-active aegis` and `aegis list`: every app that ran before must be Running. Apps restart briefly during an upgrade; warn me before you run it. See docs/upgrading.md.
```

### Prompt: uninstall

```text
Uninstall Aegis from this server, after asking me to confirm. For a server install: `systemctl disable --now aegis aegis-update.path`, remove the unit files from /etc/systemd/system/, and remove /usr/local/bin/{aegis,aegis-daemon,aegis-cli,aegis-tui}. Ask separately before deleting /var/lib/aegis (it holds every app, release and secret) and /etc/aegis. See docs/install.md#uninstall.
```

---

## 2. Register and deploy an app

### Prompt: deploy a new app from a git repository

```text
Deploy <repository URL> (branch <branch>) as an Aegis app named <name>.

1. Clone the repo into a scratch directory (read-only look) and tell me the runtime, install/build/start commands and the port the app listens on.
2. Register it. With the CLI, from inside a checkout: `aegis init --name <name> --repo <url>`; or through the API as an admin (see "Sign in to the API" below): POST /api/v1/projects {"name":"<name>","repository_url":"<url>","branch":"<branch>"}.
3. The start command must listen on $PORT. If a script hard-codes a port, change the start command (for Next.js: `npx next start -p $PORT`) in Settings or `aegis.toml`.
4. Private repo: Settings → Repository → Generate deploy key (API: POST /api/v1/projects/<name>/deploy-key). Show me the PUBLIC key and the GitHub "Add deploy key" URL (https://github.com/<owner>/<repo>/settings/keys/new, write access OFF). Wait for me to add it, then test it (POST /api/v1/projects/<name>/deploy-key/test).
5. Put plain settings in Variables and secrets (passwords, tokens) in Secrets; never in aegis.toml. Show names only.
6. `aegis validate`, then `aegis deploy <name>`, then `aegis logs <name>`.

Done when: `aegis list` shows <name> Running, the health check passes, and `curl -sI http://127.0.0.1:<port>/` answers (or the domain does, if mapped).
```

### Sign in to the API (for steps the CLI can't do)

Admin-only actions (secrets, domains, deploy keys, users, hiding apps) need a signed-in **user**, not an API token. For an agent:

```bash
# Ask the human for the admin password; do not echo it.
read -rs AEGIS_PW
curl -fsS -c /tmp/aegis.jar -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$AEGIS_PW\"}" \
  http://127.0.0.1:8420/api/v1/auth/login | tee /tmp/aegis-login.json >/dev/null
CSRF=$(python3 -c 'import json;print(json.load(open("/tmp/aegis-login.json"))["csrf_token"])')
# GET needs only the cookie; changes also need the CSRF header and JSON content type:
curl -fsS -b /tmp/aegis.jar -H "X-CSRF-Token: $CSRF" -H 'Content-Type: application/json' \
  -X POST -d '{}' http://127.0.0.1:8420/api/v1/projects/<name>/updates/check
rm -f /tmp/aegis.jar /tmp/aegis-login.json   # when finished
```

For CI-style automation, create a **deploy-scoped token** instead (can deploy, roll back, start/stop; cannot change secrets, domains, users or hide apps): `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin create-token --name <ci-name> --scope deploy` (the token is printed once; send `Authorization: Bearer <token>`).

---

## 3. Migrate existing apps onto Aegis

Principle: **run Aegis next to the old setup, verify, then switch traffic.** Never stop PM2/systemd first. Full background: [docs/migration_guide.md](docs/migration_guide.md).

### Prompt: inventory the server before migrating

```text
Inventory what runs on this server so we can plan an Aegis migration. Read-only; change nothing.
- PM2: `pm2 jlist` (name, script, cwd, args, interpreter, instances, exec_mode, status, env KEY NAMES only, port if visible) and ecosystem files (`ls ecosystem.config.*`).
- systemd: `systemctl list-units --type=service --state=running` and for app units `systemctl cat <unit>` (ExecStart, WorkingDirectory, Environment/EnvironmentFile NAMES, Restart, User).
- Reverse proxy: nginx/Caddy site files mapping domains to local ports (`nginx -T | grep -E 'server_name|proxy_pass'`).
- Ports in use: `ss -ltnp`.
- Where the code lives (git checkouts: `git -C <dir> remote -v`, `git -C <dir> rev-parse --abbrev-ref HEAD`).
Produce a table: app | manager | directory | start command | port | domain | repo+branch | private repo? | env file | notes (cluster mode, cron, watch, fixed ports). Flag risks: fixed ports, .env files ignored by git, NODE_ENV=production during build, secrets in config. Do not print secret values.
```

### Prompt: migrate one PM2 app (behind nginx)

```text
Migrate the PM2 app <pm2-name> (directory <dir>, domain <domain>, currently proxied by nginx to 127.0.0.1:<old-port>) to Aegis with no downtime. Follow docs/migration_guide.md section 5 exactly.

1. Enable Aegis's own proxy behind nginx in /etc/aegis/aegis.toml (ask me first, then `sudo systemctl restart aegis`; run `aegis-daemon check-config` before the restart):
     [proxy]
     enabled = true
     host = "127.0.0.1"
     http_port = 8081
     https_port = 8444
     trusted_proxies = ["127.0.0.1", "::1"]
   Leave [proxy.acme] off (nginx has the certificates).
2. In <dir>: `aegis migrate pm2 --app <pm2-name> --dry-run`, show me the result and every warning, then write it (`aegis migrate pm2 --app <pm2-name>`), then `aegis init --name <name>` and `aegis validate`.
3. Fix aegis.toml: start command must use $PORT (e.g. `npx next start -p $PORT`); remove secrets from [env] (move to Secrets in the dashboard); remove NODE_ENV=production unless the build works with it.
4. Env file: if PM2 used a .env that git ignores, either point the app's Env file at it (allow its directory under [apps] env_file_dirs, `chgrp aegis` + `chmod 640` the file, check with `namei -l`) or import it in Settings → Environment → Import .env. Show variable names only.
5. Map the domain BEFORE the first deploy: POST /api/v1/projects/<name>/domains {"domain":"<domain>","tls":"off"} (signed-in admin; tokens can't). This gives each release its own port so it can start while PM2 still holds <old-port>.
6. `aegis deploy <name>`, then test through Aegis's proxy exactly as nginx will:
     curl -sI -H "Host: <domain>" -H "X-Forwarded-Proto: https" http://127.0.0.1:8081/
   Check `aegis logs <name>`.
7. STOP and ask me before switching nginx. When I agree: change `proxy_pass` from http://127.0.0.1:<old-port> to http://127.0.0.1:8081 (keep the Host / X-Forwarded-* / Upgrade headers from the guide), `sudo nginx -t && sudo systemctl reload nginx`, check the site, then `pm2 stop <pm2-name> && pm2 save`.
8. State the way back (docs/migration_guide.md section 5.7) in your report.

Done when: the domain serves from Aegis, `aegis list` shows the app Running, PM2's copy is stopped (not deleted), and nginx -t passes.
```

### Prompt: migrate a systemd service

```text
Migrate the systemd unit <unit>.service to Aegis. Read the unit (`systemctl cat <unit>`), run `aegis migrate systemd --service /etc/systemd/system/<unit>.service --dry-run` in the app's directory, show the result and warnings, then write it, `aegis init --name <name>`, `aegis validate`. Move EnvironmentFile secrets to Secrets (names only in your report), make the start command use $PORT, and map a domain on Aegis's proxy if the app needs zero-downtime switches. Keep the systemd unit running until the Aegis copy is verified on a different port; then, after I confirm, `systemctl disable --now <unit>`. Never delete the unit file.
```

### Prompt: migrate many PM2 apps from a folder (scan and import)

```text
Our PM2 apps are git checkouts under <folder, e.g. /opt/github>. Register all of them in Aegis without deploying.
1. Ask me before editing /etc/aegis/aegis.toml; add
     [apps]
     import_dirs = ["<folder>"]
     env_file_dirs = ["<folder>"]     # optional: use each app's .env as its Env file
   then `sudo systemctl restart aegis`. Make the folders readable by user `aegis` (`namei -l <folder>/<app>/package.json`; fix with chgrp/chmod 640 only on the files that need it, after telling me).
2. Sign in as an admin (see "Sign in to the API"), POST /api/v1/apps/scan {"path":"<folder>"} and show me a table of candidates: name, runtime, start command, port, repo+branch, deploy_key status, .env variable names, warnings.
3. Ask which to import. Then POST /api/v1/apps/import {"apps":[{"name":..,"repository_url":..,"branch":..,"source_subdir":..}]}; report each result and its next_steps.
4. For every app with deploy_key "required": generate a deploy key, show me the public key and the GitHub add-key URL, wait for my confirmation, then test it.
5. Ports: before each first deploy either map its domain (own port per release) or `pm2 stop` that app (ask me). Otherwise the deploy stops with "port N is already in use" and nothing changes.
Do not deploy anything until I say so.
```

### Prompt: roll back a migration

```text
Roll the app <name> back to PM2/nginx: `pm2 start <pm2-name>` (or `pm2 resurrect`) and wait until it answers on port <old-port>; set nginx `proxy_pass` back to http://127.0.0.1:<old-port>; `sudo nginx -t && sudo systemctl reload nginx`; then `aegis stop <name>` so both copies do not run jobs; `pm2 save`. Report what is serving traffic now. Do not delete the Aegis app.
```

---

## 4. Manage access: user accounts, per-user apps, hidden apps

Roles: **viewer** (read), **deployer** (also deploy, roll back, start/stop), **admin** (everything). On top of roles, an admin can **limit an account to some apps** and **hide an app behind a password**. See [docs/security.md](docs/security.md#limiting-apps-per-user-and-hidden-apps).

### Prompt: add an account that sees only some apps

```text
Create the account <username> with role <viewer|deployer> that can see ONLY these apps: <app1, app2>. (Use "all apps" instead if I say so.)
Via the dashboard (Account → Users → Add user → Apps) or the API as a signed-in admin:
  POST /api/v1/users {"username":"<username>","password":"<random 24+ chars>","role":"<role>","apps":["<app1>","<app2>"]}
For all apps omit "apps" (or send "all_apps": true). Generate the password with `openssl rand -base64 24`; do not print it in logs; hand it to me once so I can pass it on. Then verify as that user (sign in with the new account): GET /api/v1/projects lists only those apps, and GET /api/v1/projects/<other-app> returns 404. Admins can't be limited (an admin role with "apps" is refused). Apps added later are NOT included for limited users until you add them.
```

### Prompt: change which apps an account sees

```text
Change <username>'s apps. To limit: PUT /api/v1/users/<username> {"apps":["<app1>","<app2>"]} (replaces the list; [] = none). To give back everything: {"all_apps": true}. Takes effect in their open sessions immediately. Verify as described above and report the final list (GET /api/v1/users).
```

### Prompt: hide an app behind a password

```text
Hide the app <name> behind a password.
1. Generate a strong password (`openssl rand -base64 24`, at least 12 characters). Hand it to me once; do not store it in files or logs. It CANNOT be recovered.
2. As a signed-in admin: PUT /api/v1/projects/<name>/hide {"password":"<password>"}  (dashboard: app → Settings → Hide this app).
3. Verify with a second session: the app is missing from GET /api/v1/projects and GET /api/v1/projects/<name> is 404 until POST /api/v1/projects/<name>/unlock {"password":"..."} succeeds (dashboard: Overview → Unlock hidden app). Unlocks last until that session ends.
Notes: hiding affects signed-in dashboard/API sessions only. API tokens, GitHub webhooks, schedules and the local CLI still reach the app. If the password is lost, a server admin can run `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin unhide-app --project <project-id>` (project id from `aegis list`).
```

### Prompt: stop hiding an app

```text
Unlock the app in an admin session (POST /api/v1/projects/<name>/unlock) and then DELETE /api/v1/projects/<name>/hide. Verify it shows in GET /api/v1/projects for other sessions that are allowed to see it.
```

### Prompt: audit who has access to what

```text
Report access: GET /api/v1/users (username, role, all_apps, apps), GET /api/v1/tokens (names and scopes, never the token strings), and which apps are hidden (GET /api/v1/projects shows `protected: true` for hidden apps you have unlocked). Recent access events: GET /api/v1/events?limit=500, keep types UserCreated, UserRoleChanged, UserAppsChanged, UserDeleted, AppHidden, AppUnhidden, AppUnlocked, AppUnlockFailed, UserLoginFailed. Flag: admins that are not needed, tokens never used, repeated AppUnlockFailed / UserLoginFailed, limited users missing a newly added app.
```

---

## 5. Operate and troubleshoot

### Prompt: health check of an Aegis install

```text
Check Aegis health and report: `aegis --version`; `systemctl status aegis --no-pager`; `curl -fsS http://127.0.0.1:8420/healthz`; `aegis status`; `aegis list`; `aegis doctor`; disk free on /var/lib/aegis (`df -h /var/lib/aegis`); last 50 daemon log lines (`journalctl -u aegis -n 50 --no-pager`); and for any app that is not Running `aegis incident <app> --since 24h`. Summarise problems and propose fixes; do not apply any without my OK.
```

### Prompt: a deploy failed

```text
The deployment of <app> failed. Run `aegis replay <deployment-id or version>` (or `aegis incident <app> --since 24h`) and show the failing stage and the log tail. Typical causes: NODE_ENV=production during `npm ci` (devDependencies skipped), a fixed port instead of $PORT, a ".env that git ignores", "port N is already in use" (old process still running), or a health check URL that needs a longer `health_check_timeout_secs`. The live release keeps serving after a failed deploy; confirm that with `aegis list`. Propose the smallest fix; apply it only after my OK, then redeploy.
```

### Prompt: back up and restore

```text
Backup: `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin backup` (add `--no-releases` for a small one) and print the archive path, not its contents. It holds password hashes and the secrets key: do not upload or paste it. Restore (ask me first; the daemon must be stopped): `sudo systemctl stop aegis; sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin restore <archive>; sudo systemctl start aegis`. See docs/disaster_recovery.md.
```

### Lost admin password

```text
Reset it on the server: `sudo -u aegis AEGIS_CONFIG=/etc/aegis/aegis.toml aegis-daemon admin reset-password` (add `--user NAME` for another account). The new password is printed once; tell me to read it from the terminal; do not copy it anywhere.
```

---

## Agent checklist (for any change)

- [ ] Read the relevant doc section first; quote the command you will run.
- [ ] Old setup still running until verified; way back stated.
- [ ] No secret values in output, files, commits or chat (names only).
- [ ] After the change: the *Done when* checks, with their real output.
- [ ] Report: what changed, what is verified, what is left for the human (passwords to read, keys to add on GitHub, nginx reload to approve).
