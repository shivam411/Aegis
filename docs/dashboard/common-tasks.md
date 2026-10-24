# Common tasks

Step-by-step recipes. The role needed is in brackets.

## Add an app and deploy it (admin)

1. **Overview → + New app** (or <kbd>n</kbd>).
2. Choose **Git repository** and enter the URL and branch, or **Directory on this server** and its path. Click **Detect settings →**.
3. Check the commands, **Port** and **Health check URL**. Keep **Deploy right away** ticked and click **Create app**.
4. Watch the seven stages on the Deploy tab. When the card says "… is live", the app is running.

Details: [New app](new-app.md).

## Deploy a new version (deployer)

1. Push your change to the app's branch (git apps), or update the directory (directory apps).
2. On the [Overview](overview.md), click **Deploy** on the app's card. Or open the app, and on the **Deploy** tab pick a **Branch**, **Commit** or **Strategy** and click **Deploy**.
3. Follow the pipeline. If the new release fails its health check, the previous release keeps serving and the deployment shows as failed or rolled back; open **Build log** to see why.

To deploy automatically, set up the [GitHub webhook](settings.md#github-webhook) or a [daily deploy](settings.md#daily-deploy) (admin).

## Roll back (deployer)

1. Open the app and go to the **Releases** tab (<kbd>2</kbd>).
2. Click **Roll back** on the release you want and confirm.
3. When it shows **Live**, the app runs that release again. No rebuild is needed.

Details: [Releases](releases.md).

## Find out why a deployment failed (viewer)

1. **Deployments** (<kbd>g</kbd> <kbd>d</kbd>), set **Status** to **Failed**, click the deployment.
2. Read the red error and the stage marked ✕, then the **Build log**.

Details: [Deployment details](deployments.md#deployment-details).

## Read and search logs (viewer)

1. Open the app, **Logs** tab (<kbd>3</kbd>).
2. Press <kbd>/</kbd> and type to filter; choose **stderr** to see only errors.
3. **Pause** to read without the view moving; **Download** to save what's loaded.

Details: [Logs](logs.md).

## Set CPU and memory limits (admin)

1. Open the app, **Resources** tab (<kbd>4</kbd>).
2. Under **Limits**, untick **No limit** for CPU or Memory and drag the slider (or type MiB in the box). It applies when you let go, to the running app, without a restart.
3. Watch the charts: the dashed red line is the new limit. If **CPU throttled** stays high or **OOM kills** goes up, raise the limit.

Details: [Resources](resources.md#limits).

## Change a setting or an environment variable (admin)

1. Open the app, **Settings** tab (<kbd>5</kbd>).
2. Change the field (or **Add variable**) and click **Save settings**.
3. Check the `aegis.toml` diff and confirm with **Save settings**.
4. Deploy, because settings apply to the next deployment.

## Give a slow app more time to start (admin)

1. Open the app, **Settings** tab, **Build & run** card.
2. Set **Health check timeout (seconds)**, e.g. `90` for an app that needs 75 s to answer (default 30). **Drain timeout** is how long a stopping release gets to finish requests.
3. **Save settings**, confirm the `aegis.toml` diff, and deploy. The dashboard value wins over `aegis.toml`, so this works for apps deployed from a server folder or a repository you can't change.

## Change where an app's code comes from (admin)

1. Open the app, **Settings** tab, **Repository** card, **Change source**.
2. Pick **Git repository** (URL and branch) or **Folder on this server** (its path), and optionally an **App folder**.
3. **Save source** and confirm. The next deployment uses it. Details: [Settings](settings.md#change-source).

## Delete an app (admin)

1. Open the app, **Settings** tab, **Danger zone** card at the bottom.
2. Click **Delete app**, type the app's name, and click **Delete this app**. Tick **Keep the release directories and logs** to leave them on the server.
3. Its processes stop, its domains are freed and it disappears; its history stays in Events. Details: [Settings](settings.md#delete-the-app).

## Add a secret (admin)

1. **Settings → Secrets**: enter the **Name** (for example `DATABASE_URL`) and **Value**, click **Save secret**.
2. Accept the offer to restart the app, so it picks up the new value.

## Put the app on a domain (admin)

1. Point the domain's DNS at the server, and make sure the built-in proxy is enabled (`[proxy] enabled = true`).
2. **Settings → Domains**: enter the domain, choose **Automatic HTTPS (Let's Encrypt)**, click **Add domain**.
3. Wait until the certificate column says **HTTPS**.

Details: [Domains](settings.md#domains).

## Run several instances (admin)

1. Map a [domain](settings.md#domains) to the app first.
2. **Settings → Build & run**: set **Instances** to 2 or more, optionally **Default strategy** to Rolling or Blue-green, and save.
3. Deploy. The Deploy tab then shows the [Instances card](docker-and-instances.md#instances).

## Add a user (admin)

1. **Account → Users**: enter a **Username**, choose a **Role** (viewer, deployer or admin) and click **Add user**.
2. Copy the password that appears (it is shown once) and give it to the person. They can change it under Account → Password.

To change someone's role later, use the **Role** menu in their row. Details: [Users](account.md#users).

## Create an API token for CI (admin)

1. **Account → API tokens**: enter a **Name** such as `ci-deploy`, choose scope **deploy**, click **Create token**.
2. Copy the token (shown once) into your CI secrets. Use it as `Authorization: Bearer <token>`.
3. If it leaks, click **Revoke**.

## Take a backup (admin)

1. **Account → Backups**: choose whether to **Include releases**, then click **Back up now**.
2. Click **Download** to keep a copy off the server.

To restore, follow the commands under **Restore** on the same card. Details: [Backups](account.md#backups).

## Update Aegis (admin)

1. When the "Aegis vX is available" notice appears, read the **Release notes**.
2. Click **Upgrade now** and confirm. A backup is taken first; apps restart with Aegis.

Details: [Aegis updates](updates.md).

## See who did something (viewer)

1. **Events** (<kbd>g</kbd> <kbd>e</kbd>).
2. Pick the **App** and **Category**, or type a name or version into **Filter by text**.
3. Each entry ends with "by user:…" or "by token:…".

Details: [Events](events.md).
