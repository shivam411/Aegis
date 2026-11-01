# Common tasks

Short recipes. Role in brackets.

## Add an app and deploy it (admin)

1. **Overview → + New app** (<kbd>n</kbd>).
2. Choose **Git repository** (URL, branch) or **Directory on this server**; **Detect settings →**.
3. Check commands, **Port**, **Health check URL**. Keep **Deploy right away** ticked; **Create app**.
4. Watch the Deploy tab until it says "… is live".

Details: [New app](new-app.md).

## Deploy a new version (deployer)

1. Push to the app's branch (or update the directory).
2. **Deploy** on the app's card in the [Overview](overview.md); or on the **Deploy** tab pick **Branch**, **Commit**, **Strategy**, then **Deploy**.
3. If the health check fails, the previous release keeps serving; open **Build log** for why.

Automate with a [GitHub webhook](settings.md#github-webhook) or [daily deploy](settings.md#daily-deploy) (admin).

## Roll back (deployer)

1. App → **Releases** (<kbd>2</kbd>).
2. **Roll back** on the release you want; confirm.
3. When it shows **Live**, you're done. No rebuild.

Details: [Releases](releases.md).

## Find out why a deployment failed (viewer)

1. **Deployments** (<kbd>g</kbd> <kbd>d</kbd>), **Status** **Failed**, open it.
2. Read the red error, the ✕ stage, then **Build log**.

Details: [Deployment details](deployments.md#deployment-details).

## Read and search logs (viewer)

1. App → **Logs** (<kbd>3</kbd>).
2. <kbd>/</kbd> to filter; **stderr** for errors only.
3. **Pause** to read; **Download** to save.

Details: [Logs](logs.md).

## Set CPU and memory limits (admin)

1. App → **Resources** (<kbd>4</kbd>) → **Limits**.
2. Untick **No limit**, drag the slider (or type MiB). Applies on release, no restart.
3. If **CPU throttled** stays high or **OOM kills** rise, raise the limit.

Details: [Resources](resources.md#limits).

## Change a setting or an environment variable (admin)

1. App → **Settings** (<kbd>5</kbd>).
2. Edit the field (or **Add variable**); **Save settings**.
3. Check the `aegis.toml` diff; confirm.
4. Deploy: settings apply to the next deployment.

## Give a slow app more time to start (admin)

1. **Settings → Build & run**.
2. Set **Health check timeout (seconds)**, e.g. `90` (default 30). **Drain timeout** is how long a stopping release gets to finish requests.
3. **Save settings**, confirm, deploy. The dashboard value wins over `aegis.toml`.

## Change where an app's code comes from (admin)

1. **Settings → Repository → Change source**.
2. Pick **Git repository** (URL, branch) or **Folder on this server**, optionally an **App folder**.
3. **Save source**; confirm. Used from the next deployment. See [Settings](settings.md#change-source).

## Delete an app (admin)

1. **Settings → Danger zone** (bottom).
2. **Delete app**, type the name, **Delete this app**. Tick **Keep the release directories and logs** to keep them.
3. Processes stop and domains are freed; history stays in Events. See [Settings](settings.md#delete-the-app).

## Add a secret (admin)

1. **Settings → Secrets**: **Name** (e.g. `DATABASE_URL`), **Value**, **Save secret**.
2. Accept the restart offer.

## Put the app on a domain (admin)

1. Point DNS at the server; enable the proxy (`[proxy] enabled = true`).
2. **Settings → Domains**: domain, **Automatic HTTPS (Let's Encrypt)**, **Add domain**.
3. Wait for **HTTPS** in the certificate column.

Details: [Domains](settings.md#domains).

## Run several instances (admin)

1. Map a [domain](settings.md#domains) first.
2. **Settings → Build & run**: **Instances** 2 or more; optionally **Default strategy** Rolling or Blue-green; save.
3. Deploy. See the [Instances card](docker-and-instances.md#instances).

## Add a user (admin)

1. **Account → Users**: **Username**, **Role**, **Add user**.
2. Copy the password (shown once) and pass it on. They can change it under Account → Password.

Change a role later with the **Role** menu in their row. Details: [Users](account.md#users).

## Create an API token for CI (admin)

1. **Account → API tokens**: **Name** (e.g. `ci-deploy`), scope **deploy**, **Create token**.
2. Copy it (shown once) into CI secrets; send `Authorization: Bearer <token>`.
3. **Revoke** if it leaks.

## Take a backup (admin)

1. **Account → Backups**: optionally **Include releases**; **Back up now**.
2. **Download** for an off-server copy.

Restore with the commands under **Restore**. Details: [Backups](account.md#backups).

## Update Aegis (admin)

1. When "Aegis vX is available" appears, read the **Release notes**.
2. **Upgrade now**, confirm. A backup is taken first; apps restart.

Details: [Aegis updates](updates.md).

## See who did something (viewer)

1. **Events** (<kbd>g</kbd> <kbd>e</kbd>).
2. Pick **App** and **Category**, or use **Filter by text**.
3. Entries end with "by user:…" or "by token:…".

Details: [Events](events.md).
