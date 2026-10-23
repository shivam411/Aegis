# New app

**+ New app** on the [Overview](overview.md) (or the <kbd>n</kbd> key) opens a two-step wizard. Only admins can add apps.

## 1. Source

![Step 1: where the code comes from](../images/dashboard/new-app-source.png)

Choose where the code comes from:

- **Git repository.** Enter the **Repository URL** (https, ssh, or a path on the server the daemon can read) and the **Branch** (`main` by default). Every deployment clones that branch, or a commit you pick.
- **Directory on this server.** Enter the path. Every deployment copies the directory as it is at that moment.
- **Scan a folder on the server.** Finds several existing apps at once and imports them; see [below](#scan-a-folder-on-the-server).

**App folder in the source** is optional, for monorepos: only that folder (for example `apps/web`) is built and run. Files outside it (a root lockfile, shared workspace packages) are not part of the release, so the folder must build on its own.

**App name** is optional. Without it, the name comes from the repository or directory. The name is used in URLs and in CLI commands such as `aegis deploy shop`.

Click **Detect settings →**. Aegis inspects the code to find the runtime (Node.js, Python, Rust, Go, Bun or Deno) and suggests commands. An `aegis.toml` in the code takes precedence over detection.

### Private repositories

If Aegis can't read a GitHub repository, the wizard asks GitHub (without credentials) whether it is public. When it isn't, it offers **Create the app and set up a deploy key**: the app is created (nothing is deployed), and the next step shows its deploy key panel, the same as [Settings → Repository](settings.md#repository):

1. **Generate deploy key.** Aegis creates an Ed25519 key pair for this app and shows the public key and its fingerprint. The private key stays on the server, encrypted.
2. **Copy** the public key and open the link to the repository's *Add deploy key* page (`https://github.com/<owner>/<repo>/settings/keys/new`). Give it a title, paste the key, leave **Allow write access** unticked, and click **Add key**.
3. **Test access.** Aegis runs `git ls-remote` with the key and shows the result, or git's error with what to do about it.
4. **Detect settings →** clones with the key and continues to step 2.

An `https://github.com/…` URL is cloned over SSH (`git@github.com:owner/repo.git`) once the app has a key, because deploy keys only work over SSH. For other git servers use an SSH URL, and add the server's host key to `[git] known_hosts_file` ([configuration](../configuration.md#git-servers-git)).

## 2. Review

![Step 2: the detected settings, ready to create the app](../images/dashboard/new-app-review.png)

The pill in the header says what was detected. Check and change the fields:

| Field | Meaning |
| :--- | :--- |
| **App name** | Required. |
| **Install command** | Installs dependencies, for example `npm ci`. |
| **Build command** | Builds the app, for example `npm run build`. |
| **Test command** | Runs tests. A failure stops the deployment before anything goes live. |
| **Start command** | Starts the app, for example `node server.js`. |
| **Port** | The port the app listens on. The app also gets it as `PORT`. |
| **Health check URL** | A new release goes live only once this URL answers with a status below 500 (and the answer comes from the new release: see [Ports and health checks](../configuration.md#domains-and-the-built-in-reverse-proxy-proxy)). Changing **Port** moves the port in this URL along with it. |

**Hard-coded ports.** When the `start` script in `package.json` fixes its port (`next start -p 4000`, `--port 4000`, `PORT=4000 node server.js`), a note says so and the start command is already changed to read `$PORT` (`npx next start -p $PORT`), with **Port** set to 4000. Aegis tells every release its port in `PORT` (behind the built-in proxy each release gets its own), so an app that ignores it can't be switched without downtime.

Only the values you change (or that the wizard changed for you, like the start command above) are saved as dashboard settings. Everything else keeps following the app's `aegis.toml` and auto-detection, so later changes to the repository still apply. You can change all of these later in the [Settings tab](settings.md).

Leave **Deploy right away** ticked to start the first deployment immediately. Click **Create app**: the [app page](app.md) opens and the pipeline runs on the [Deploy tab](deploy.md). **← Back** returns to step 1.

## Scan a folder on the server

For a server that already runs apps from git checkouts (for example with PM2, in `/opt/github/<repo>`), **Scan a folder on the server** finds them and imports the ones you pick, without deploying them. It needs the folder to be allowed in the daemon configuration (`[apps] import_dirs`, or `env_file_dirs`; see [configuration](../configuration.md#app-settings-apps)); the allowed folders are listed under the path field, and when there are none the wizard says how to add one.

Enter a folder and click **Scan →**. Aegis looks up to two levels deep for app folders (with a `package.json`, `pyproject.toml`, `requirements.txt`, `Cargo.toml`, `go.mod`, `pom.xml`, `build.gradle`, `Dockerfile` or `aegis.toml`), including the apps of a monorepo in `apps/`, `packages/` and `services/` (listed with their app folder). It only reads; see [what it reads](../security.md#scanning-folders-on-the-server).

Each row shows:

| Column | Meaning |
| :--- | :--- |
| ☐ | Import this app. Apps already registered (same repository, branch and folder, or the same directory) say *Already imported as …* and can't be selected. |
| **Name** | Editable; defaults to the folder name (`<repo>-<app>` for monorepo apps). |
| **Source** | The folder, the checkout's `origin` URL and branch (read from `.git`; credentials in the URL are dropped), and whether to **Clone the repository** or **Copy this folder** at each deployment. **Check** asks GitHub whether the repository is public; a private one needs a deploy key. |
| **Start command / Port** | Editable. A start script that fixes its port is already changed to read `$PORT`. |
| **Env file** | The app's `.env` with its variable **names** (never values). Ticked: use it as the app's Env file (only possible when its folder is in `[apps] env_file_dirs`). |
| **Warnings** | *Port in use* (with the process, when visible; probably the current PM2 copy), *deploy key needed* (SSH remote) or *may be private* (GitHub over HTTPS), *.env not readable* (with the `chgrp aegis … && chmod 640 …` fix), *hard-coded port*, *no remote*. |

**Import selected** registers the apps (no deployment, whatever you choose) and lists the next steps for each: add a deploy key for private repositories, set the Env file or Variables/Secrets, map a domain so each release gets its own port, and stop the old process if it still holds the port. Then open each app and deploy it when ready.
