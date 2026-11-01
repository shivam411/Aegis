// Help text for the dashboard: the "?" button on every page and card, and
// the first-run tour. Kept apart from app.js so the wording can be checked
// against the guide in docs/dashboard/ in one place. Each `docs` value is a
// page (and heading anchor) in that guide.

export const DOCS_BASE = 'https://github.com/shivam411/Aegis/blob/main/docs/dashboard/';

/**
 * key -> {title, summary, controls: [[control, what it does]], roles, docs}
 * `roles` says who can use the section; the server enforces it either way.
 */
export const HELP = {
  // --- Overview -----------------------------------------------------------
  overview: {
    title: 'Overview',
    summary: 'Server load and every app with its live state. Updates live.',
    controls: [
      ['+ New app', 'Opens the new app wizard (admins).'],
      ['App cards', 'Click a name to open the app page.'],
    ],
    roles: 'Everyone sees it. Deploy/Start/Stop/Restart: deployer. + New app: admin.',
    docs: 'overview.md',
  },
  'overview.host': {
    title: 'Host resources',
    summary: 'CPU, memory, disk and load of the server, every 5 seconds. Bars turn amber at 75%, red at 90%.',
    controls: [
      ['CPU', 'Percent of all cores, with core count.'],
      ['Memory', 'Used of total, plus swap.'],
      ['Disk', 'Fullness of the disk holding Aegis data.'],
      ['Load', '1, 5 and 15 minute averages, and uptime.'],
    ],
    roles: 'Everyone.',
    docs: 'overview.md#host-tiles',
  },
  'overview.apps': {
    title: 'Apps',
    summary: 'Each app with status, version, uptime, restarts and CPU/memory against its limits. A blue Deploying pill means a deployment is running.',
    controls: [
      ['Deploy', 'Deploys the latest code with the default strategy.'],
      ['Start / Stop / Restart', 'Controls the process. Stop and Restart ask first.'],
      ['Logs', 'Opens the live log.'],
      ['Grid / List', 'Cards or table. Remembered in this browser.'],
      ['Bulk actions', 'Tick apps, then act on all. Run in order; failures are toasted. Grouped apps go via their group.'],
    ],
    roles: 'Everyone sees cards. Actions need deployer or admin.',
    docs: 'overview.md#app-cards',
  },
  wizard: {
    title: 'New app',
    summary: 'Give the code location; Aegis suggests commands, port and health check. Review, then create. Or scan a server folder to import several apps.',
    controls: [
      ['Git repository / Directory', 'A repository is cloned each deploy; a directory is copied.'],
      ['Scan a folder', 'Imports apps from a folder in [apps] import_dirs or env_file_dirs, without deploying.'],
      ['App folder in the source', 'Monorepos: build and run only this folder.'],
      ['Private repository', 'Creates the app and a deploy key first, then detects.'],
      ['Detect settings', 'Fills step 2. A fixed port (next start -p 4000) becomes $PORT.'],
      ['Deploy right away', 'Deploys as soon as the app is created.'],
    ],
    roles: 'Admins only.',
    docs: 'new-app.md',
  },
  'wizard.scan': {
    title: 'Scan results',
    summary: 'Apps found, with the settings an import would save. Nothing changes until you import, and nothing is deployed. Already registered apps can\'t be selected.',
    controls: [
      ['Name / Start command / Port', 'Editable. A fixed port is already replaced with $PORT.'],
      ['Source', 'Clone the repository, or copy the folder, at each deploy.'],
      ['Check', 'Asks GitHub if the repo is public. Private needs a deploy key.'],
      ['Env file', 'Use the app\'s .env (folder must be in env_file_dirs). Only names shown.'],
      ['Warnings', 'Port in use, deploy key needed, .env unreadable by the daemon, fixed port.'],
      ['Import selected', 'Registers the apps and lists next steps.'],
    ],
    roles: 'Admins only.',
    docs: 'new-app.md#scan-a-folder-on-the-server',
  },

  // --- App page -----------------------------------------------------------
  app: {
    title: 'App page',
    summary: 'One app: status, version, uptime and source in the header; tabs below.',
    controls: [
      ['Start / Stop / Restart', 'Controls the process or container. Stop and Restart ask first.'],
      ['Tabs', 'Deploy, Releases, Logs, Resources, Settings, Activity (keys 1 to 6).'],
    ],
    roles: 'Everyone can look. Control, deploy, roll back: deployer. Settings: admin.',
    docs: 'app.md',
  },
  'app.deploy': {
    title: 'New deployment',
    summary: 'Builds and releases a new version. Git apps can deploy any branch or commit; directory apps copy the directory as it is.',
    controls: [
      ['Branch', 'Branch to build. Blank uses the app\'s branch.'],
      ['Commit', 'A SHA on that branch. Blank is the latest.'],
      ['Strategy', 'Graceful switch, Immediate, Rolling or Blue-green.'],
      ['Deploy', 'Queues it; the pipeline appears below.'],
    ],
    roles: 'Deployer or admin.',
    docs: 'deploy.md#new-deployment',
  },
  'app.pipeline': {
    title: 'Deployment progress',
    summary: 'The latest deployment, live: seven stages, timings, rollout steps, outcome and build log.',
    controls: [
      ['Stages', 'Clone, Install, Build, Test, Package, Verify (health check), Promote (go live). A dash is skipped.'],
      ['Build log', 'Build output; open while running or after a failure.'],
      ['Details', 'Opens the deployment page with all events.'],
    ],
    roles: 'Everyone.',
    docs: 'deploy.md#deployment-progress',
  },
  'app.instances': {
    title: 'Instances',
    summary: 'Shown when the app runs several copies, each with its own process, port and log. The built-in proxy spreads requests. After Blue-green, the old release may stay up for instant rollback.',
    controls: [
      ['Logs', 'Opens that instance\'s log.'],
    ],
    roles: 'Everyone.',
    docs: 'docker-and-instances.md#instances',
  },
  'app.releases': {
    title: 'Releases',
    summary: 'Builds Aegis still keeps, newest first. Live serves traffic. Roll back to an older one without rebuilding.',
    controls: [
      ['Roll back', 'Switches to that release once it passes the health check.'],
    ],
    roles: 'Everyone sees them. Rolling back: deployer or admin.',
    docs: 'releases.md',
  },
  'app.logs': {
    title: 'Logs',
    summary: 'Live output. Errors are red, warnings amber. Filter by level, time or text.',
    controls: [
      ['Search', 'Text, or a regular expression with Regex on. Press / to jump here.'],
      ['Level chips', 'Show or hide errors, warnings, info and debug lines (with counts).'],
      ['Time range', 'The last few minutes or hours, or a custom from/to.'],
      ['Follow, Pause', 'Follow sticks to the newest line; Pause freezes the view.'],
      ['Download', 'Saves the lines you see (after filters) as a .log file.'],
    ],
    roles: 'Everyone.',
    docs: 'logs.md',
  },
  'app.resources': {
    title: 'Live usage',
    summary: 'Current CPU, memory, CPU throttling, threads, open files and OOM kills. Updates every few seconds.',
    controls: [],
    roles: 'Everyone.',
    docs: 'resources.md#live-usage',
  },
  'app.usage': {
    title: 'Usage charts',
    summary: 'CPU and memory over time. Solid line: average. Faint: peak. Dashed red: limit. Hover for values.',
    controls: [
      ['Live · 5 min / 1 hour / 24 hours / 7 days', 'Time range. Live keeps adding samples.'],
    ],
    roles: 'Everyone.',
    docs: 'resources.md#usage-charts',
  },
  'app.limits': {
    title: 'Limits',
    summary: 'Caps on CPU, memory and processes. Applied at once without restart, and on every start. Each instance gets the limit on its own.',
    controls: [
      ['CPU', 'Max cores. Tick No limit to remove.'],
      ['Memory', 'Max MiB. Above it the app restarts; at 90% it is asked to free memory.'],
      ['Processes', 'Max processes and threads, where supported.'],
    ],
    roles: 'Admins change limits; others see them read-only.',
    docs: 'resources.md#limits',
  },
  'app.settings': {
    title: 'Build & run',
    summary: 'How the app is built and started. Blank fields use aegis.toml or auto-detection. Saving shows the aegis.toml change first and applies from the next deployment.',
    controls: [
      ['Install / Build / Test / Start command', 'Shell commands for the pipeline and start.'],
      ['Port / Health check URL', 'Where the app listens; the URL must answer (below 500) before go-live.'],
      ['No HTTP health check', 'Healthy while the process stays up.'],
      ['Default strategy / Instances', 'Strategy when none is picked; copies to run (more than one needs a domain).'],
      ['Live URL check', 'URL requested after go-live. If not up in time, rolls back and fails the deployment.'],
      ['Health check timeout', 'Seconds to become healthy (1-3600, default 30; [deploy] health_check_timeout_secs).'],
      ['Drain timeout', 'Seconds to finish requests after SIGTERM before kill (0-3600, default 10; [deploy] drain_timeout_secs).'],
      ['Environment variables', 'Plain settings. Use Secrets for passwords and keys.'],
      ['View aegis.toml / Show detected defaults', 'The file the next release gets, or what detection would pick.'],
    ],
    roles: 'Admins. Others can view aegis.toml.',
    docs: 'settings.md#build-and-run',
  },
  'app.domains': {
    title: 'Domains',
    summary: 'Serve the app on your own domains via the built-in proxy, with automatic HTTPS (Let\'s Encrypt) or your certificate files. Point DNS at this server first.',
    controls: [
      ['Domain / TLS', 'Host name, and automatic HTTPS, own certificate, or HTTP only.'],
      ['Remove', 'Stops serving that domain at once.'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#domains',
  },
  'app.repository': {
    title: 'Repository',
    summary: 'Where the code comes from, plus an SSH deploy key for private repositories. The private key stays encrypted on this server; only the public key is shown.',
    controls: [
      ['Generate deploy key', 'Add the public key to the repo\'s deploy keys (GitHub: Settings → Deploy keys), write access off.'],
      ['Copy', 'Copies the public key.'],
      ['Test access', 'Runs git ls-remote and shows the result or the fix.'],
      ['Generate a new key / Delete key', 'Replace or remove; update the repo\'s deploy keys too.'],
      ['Change source', 'Switch git repository or server folder. Used from the next deployment; the key is kept.'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#repository',
  },
  'app.env': {
    title: 'Environment',
    summary: 'Variable sources, lowest to highest: env file, [env] in aegis.toml, Variables, Secrets, then PORT. Builds get the first three, never Secrets. Only names are shown.',
    controls: [
      ['Env file path', 'Absolute path, read at every build and start, never stored. Must be in [apps] env_file_dirs and readable by the daemon (chgrp aegis FILE && chmod 640 FILE).'],
      ['Status', 'Whether the file was readable just now, and why not.'],
      ['Value from / Overrides / Build', 'Which source wins, what it overrides, and whether builds see it.'],
      ['Import .env', 'Paste or choose a .env, then save each name as Variable, Secret or skip. Multi-line values must be Secrets.'],
    ],
    roles: 'Everyone sees names and sources; admins set the file and import.',
    docs: 'settings.md#environment',
  },
  'app.secrets': {
    title: 'Secrets',
    summary: 'Encrypted variables passed to the app at start. Values can be replaced, never read. Restart to apply. Not available to builds: use Variables for those (e.g. NEXT_PUBLIC_*).',
    controls: [
      ['Value', 'Saved as typed, including line breaks (PEM keys work). Masked; Show reveals.'],
      ['Save secret', 'Adds or replaces, then offers a restart.'],
      ['Replace / Delete', 'Set a new value, or remove it.'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#secrets',
  },
  groups: {
    title: 'App groups',
    summary: 'Apps from the same repository and branch, deployed together in order from one clone. "Deploy changed only" and the group webhook deploy just apps whose watch paths changed.',
    controls: [
      ['Deploy group', 'Deploys every app at the branch\'s latest commit, in order.'],
      ['Deploy changed only', 'Skips apps with no changed watched files; reasons are listed.'],
      ['Roll back group', 'Returns apps switched by the last group deployment to their previous release.'],
      ['When an app fails…', 'Later apps are skipped; ticked, already switched apps roll back.'],
    ],
    roles: 'Everyone sees groups; deployers deploy and roll back; admins edit.',
    docs: 'groups.md',
  },
  'app.updates': {
    title: 'New changes',
    summary: 'Whether the live release is behind its source. Git apps are fetched every few minutes, after webhooks, and on Check now. Folder apps read git HEAD and count uncommitted files. A failed check never blocks deploys.',
    controls: [
      ['Check now', 'Fetches or reads right away.'],
      ['Deploy latest', 'Deploys the newest commit (grouped apps deploy via their group).'],
      ['N changed files', 'Files changed since live; grouped apps show only watch-path matches.'],
    ],
    roles: 'Everyone sees changes; deployers check and deploy.',
    docs: 'deploy.md#new-changes',
  },
  'groups.members': {
    title: 'Apps of the group',
    summary: 'Apps in deploy order, each with watch paths (globs like frontend_v2/** and shared/**). No watch paths means any change counts.',
    controls: [
      ['↑ ↓', 'Change deploy order (e.g. API before front end).'],
      ['Watch paths / Save', 'Comma-separated globs: * and ? in a folder name, ** across folders; a folder name means all under it.'],
      ['Add app / Remove', 'Only apps from the group\'s repository and branch; one group per app.'],
    ],
    roles: 'Admins change apps and watch paths.',
    docs: 'groups.md#the-group-page',
  },
  'groups.webhook': {
    title: 'Group webhook',
    summary: 'One GitHub webhook for the group: a push to its branch deploys the changed apps. Members\' own webhooks are ignored, so nothing deploys twice.',
    controls: [
      ['Generate secret / Rotate secret', 'Shown once. Add the Payload URL and secret in GitHub → Settings → Webhooks.'],
      ['Send test delivery', 'Sends a signed ping.'],
    ],
    roles: 'Admins, signed in.',
    docs: 'groups.md#the-group-page',
  },
  'app.jobs': {
    title: 'Jobs',
    summary: 'Commands run in the live release on a cron schedule (UTC) or on demand: migrations, cleanups, reports. They get the app\'s variables and secrets, run with sh, and are killed after their timeout. Output of the last 50 runs is kept.',
    controls: [
      ['Run now', 'Starts it now (deployers, admins). Never twice at once.'],
      ['Pause / Resume', 'Stops or restarts the schedule.'],
      ['Runs', 'History with exit codes and output tail.'],
      ['Cron', 'Five fields (minute hour day month weekday) or @hourly, @daily, @weekly, @monthly. If both day fields are set, either may match.'],
    ],
    roles: 'Everyone reads; deployers run; admins create, change, delete (not API tokens).',
    docs: 'jobs.md',
  },
  'app.hide': {
    title: 'Hide this app',
    summary: 'A hidden app disappears from lists, search and events, and answers "not found", until a session enters its password. The unlock lasts until the session ends. To limit one account to some apps, use Account → Users.',
    controls: [
      ['Password', 'At least 12 characters. Unrecoverable; setting a new one locks all sessions.'],
      ['Hide it now', 'Hides it again for your session.'],
      ['Stop hiding', 'Removes the password.'],
    ],
    roles: 'Admins, signed in (not API tokens). Anyone who may see the app can unlock it. API tokens ignore hiding.',
    docs: 'settings.md#hide-the-app',
  },
  'app.delete': {
    title: 'Danger zone',
    summary: 'Deleting stops all processes (drain timeout applies) and domains, and removes secrets, deploy key, webhook secret, daily deploy and releases. Events stay. The name and domains can be reused at once; issued certificates are kept.',
    controls: [
      ['Delete app', 'Type the app\'s name. Not possible during a deployment.'],
      ['Keep the release directories and logs', 'Leaves them in projects/<id> in the data directory.'],
    ],
    roles: 'Admins, signed in (not API tokens).',
    docs: 'settings.md#delete-the-app',
  },
  'app.schedule': {
    title: 'Daily deploy',
    summary: 'Deploys automatically once a day at a set time (UTC), e.g. nightly.',
    controls: [
      ['Time (UTC) / Branch', 'When, and which branch (git apps).'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#daily-deploy',
  },
  'app.webhook': {
    title: 'GitHub webhook',
    summary: 'Deploys on every push to the app\'s branch. Generate a secret, then add a GitHub webhook with the payload URL and secret.',
    controls: [
      ['Generate / Rotate secret', 'Shown once; rotating breaks the old one.'],
      ['Send test delivery', 'Checks the endpoint accepts a signed request.'],
      ['Recent deliveries', 'What GitHub sent and what Aegis did.'],
    ],
    roles: 'Admins. Needs a git source.',
    docs: 'settings.md#github-webhook',
  },
  'app.about': {
    title: 'About',
    summary: 'Fixed facts: id (for CLI and API), runtime, process or Docker, creation date and code source.',
    controls: [],
    roles: 'Admins.',
    docs: 'settings.md#about',
  },
  'app.activity': {
    title: 'Activity',
    summary: 'This app\'s audit log, newest first and live: deployments, releases, rollbacks, starts, crashes, setting changes. Details shows full data.',
    controls: [],
    roles: 'Everyone.',
    docs: 'activity.md',
  },

  // --- Deployments ----------------------------------------------------------
  deployments: {
    title: 'Deployments',
    summary: 'Every deployment of every app, newest first: status, release, trigger, stage reached and duration. Click a row for details.',
    controls: [
      ['App / Status filters', 'One app, or in-progress, succeeded, failed, rolled-back.'],
    ],
    roles: 'Everyone.',
    docs: 'deployments.md',
  },
  deployment: {
    title: 'Deployment details',
    summary: 'One deployment: stages, rollout steps, outcome, build log and all its events.',
    controls: [
      ['Open app', 'Goes to the app page.'],
    ],
    roles: 'Everyone.',
    docs: 'deployments.md#deployment-details',
  },

  // --- Events ---------------------------------------------------------------
  events: {
    title: 'Events',
    summary: 'Server-wide audit log with who did what: deployments, process changes, sign-ins, tokens, settings, backups, updates. New events appear on top while Live is ticked.',
    controls: [
      ['App / Category / Last N', 'Filter, and choose how many to load.'],
      ['Filter by text', 'Matches type and data. Press / to jump here.'],
      ['Live', 'Untick to freeze the list.'],
      ['Details', 'Full event data as JSON.'],
    ],
    roles: 'Everyone.',
    docs: 'events.md',
  },

  // --- Account --------------------------------------------------------------
  account: {
    title: 'Account',
    summary: 'Your password and display preferences. Admins also manage users, API tokens and backups.',
    controls: [
      ['Sign out', 'Ends this browser session.'],
    ],
    roles: 'Everyone: Password, Appearance. Admins: Users, API tokens, Backups.',
    docs: 'account.md',
  },
  system: {
    title: 'System',
    summary: 'Keep Aegis current and see what uses the disk.',
    controls: [
      ['Updates', 'Check for a new version and install it.'],
      ['Storage and cleanup', 'Disk use by category, a Clean up button, and automatic cleanup.'],
    ],
    roles: 'Admins.',
    docs: 'system.md',
  },
  'system.updates': {
    title: 'Updates',
    summary: 'Your version, the latest release, and an Upgrade button where Aegis can update itself. A backup is taken first; apps restart briefly.',
    controls: [
      ['Check now', 'Asks GitHub for the latest release.'],
      ['Upgrade', 'Backs up, downloads, verifies the signature, swaps the binary and restarts.'],
    ],
    roles: 'Admins.',
    docs: 'system.md#updates',
  },
  'system.storage': {
    title: 'Storage and cleanup',
    summary: 'What Aegis keeps on disk: releases, old builds, logs, backups, the database and temporary files. Cleaning removes only what is not needed.',
    controls: [
      ['Clean up', 'Removes old builds, old logs, extra backups and temp files. Live and rollback releases stay.'],
      ['Automatic cleanup', 'Does the same every hour. Off by default.'],
    ],
    roles: 'Admins.',
    docs: 'system.md#storage',
  },
  docker: {
    title: 'Docker',
    summary: 'Containers on this server and their logs. Shared separately: only the owner, and users the owner allows.',
    controls: [
      ['Container row', 'Click to open its live logs.'],
      ['Refresh', 'Reloads the list.'],
    ],
    roles: 'The owner, or users given Docker access in Account → Users.',
    docs: 'docker.md',
  },
  'account.users': {
    title: 'Users',
    summary: 'Everyone signs in with their own account, so the audit log shows who did what. New users and resets get a random password, shown once.',
    controls: [
      ['Role', 'Viewer reads; deployer also deploys, rolls back, starts, stops; admin does everything.'],
      ['Apps', 'All apps (including future ones) or only those ticked. Others answer "not found".'],
      ['Docker', 'Lets the user see Docker containers and logs. Off by default; the owner always can.'],
      ['Reset password', 'Signs the user out everywhere; new password shown once.'],
      ['Delete', 'Removes the user; past actions stay in the audit log. Not yourself.'],
      ['Add user', 'Creates a user with the chosen role.'],
    ],
    roles: 'Admins.',
    docs: 'account.md#users',
  },
  'account.monitoring': {
    title: 'Monitoring (Prometheus & Grafana)',
    summary: 'Optional /metrics endpoint in Prometheus format, off until enabled, plus a Grafana dashboard. Prometheus fetches it; nothing is pushed.',
    controls: [
      ['Turn metrics on / off', 'Off answers "not found".'],
      ['Download Grafana dashboard', 'JSON to import in Grafana (Dashboards → New → Import).'],
    ],
    roles: 'Admins. Scraping needs an API token (any scope) or a session.',
    docs: 'monitoring.md',
  },
  'account.tokens': {
    title: 'API tokens',
    summary: 'For the CLI, scripts and CI, sent as Authorization: Bearer. Shown once when created.',
    controls: [
      ['Scope', 'deploy can deploy, roll back, start, stop; read only looks.'],
      ['Revoke', 'Access ends immediately.'],
    ],
    roles: 'Admins.',
    docs: 'account.md#api-tokens',
  },
  'account.backups': {
    title: 'Backups',
    summary: 'Archives of the database, secrets key, certificates and optionally releases. Restore on the server with the daemon stopped; commands are shown below the list.',
    controls: [
      ['Include releases', 'Also archive built releases (large).'],
      ['Back up now', 'Creates a backup on the server.'],
      ['Download / Delete', 'Save to your computer, or remove from the server.'],
    ],
    roles: 'Admins. Backups hold password hashes and the secrets key: keep downloads safe.',
    docs: 'account.md#backups',
  },
  'account.password': {
    title: 'Password',
    summary: 'Change your password (at least 12 characters). All your sessions are signed out, including this one.',
    controls: [],
    roles: 'Everyone.',
    docs: 'account.md#password',
  },
  'account.appearance': {
    title: 'Appearance & shortcuts',
    summary: 'Theme (light, dark or system; remembered in this browser), keyboard shortcuts and the tour.',
    controls: [
      ['Theme', 'System, light or dark. Press t to toggle.'],
      ['Take the tour again', 'Replays the first-run tour.'],
    ],
    roles: 'Everyone.',
    docs: 'account.md#appearance-and-shortcuts',
  },

  // --- Shell ----------------------------------------------------------------
  update: {
    title: 'Aegis updates',
    summary: 'A new Aegis release is available. Upgrading backs up, verifies the download, swaps binaries and restarts; apps restart too, so expect a short outage.',
    controls: [
      ['Upgrade now', 'Starts the update after confirmation (admins, if self-update is supported).'],
      ['Release notes', 'What changed.'],
      ['Later', 'Hides the notice for this version in this browser.'],
    ],
    roles: 'Everyone sees the notice; only admins upgrade.',
    docs: 'updates.md',
  },
};

/**
 * The first-run tour. `target` is a CSS selector (no target: a centred
 * card); `route` the page it lives on ('app' is the first app's page);
 * `role` the least role that has the control; `needsApp` skips the step
 * while there are no apps. Steps whose target can't be found are skipped.
 */
export const TOUR = [
  {
    id: 'welcome',
    title: 'Welcome to Aegis',
    text: 'A one-minute tour. Skip it and replay any time from the ? button or Account.',
  },
  {
    id: 'nav-overview',
    target: '[data-nav="overview"]',
    title: 'Overview',
    text: 'Server health and every app with its status.',
  },
  {
    id: 'host',
    route: '/',
    target: '#host',
    title: 'Server health',
    text: 'CPU, memory, disk and load. Bars turn amber, then red, as they fill.',
  },
  {
    id: 'new-app',
    route: '/',
    role: 'admin',
    target: '[data-tour="new-app"]',
    title: 'Add an app',
    text: 'Point Aegis at a git repository or server directory. It detects how to build and run it.',
  },
  {
    id: 'deploy',
    route: '/',
    role: 'deployer',
    needsApp: true,
    target: '[data-app] [data-action="deploy"]',
    title: 'Deploy',
    text: 'Builds the latest code, health-checks it, switches traffic. If the check fails, the old release keeps running.',
  },
  {
    id: 'tabs',
    route: 'app',
    needsApp: true,
    target: '.tabs',
    title: 'Inside an app',
    text: 'Deploy, Releases (roll back), Logs, Resources, Settings, Activity. Keys 1 to 6 switch.',
  },
  {
    id: 'help',
    target: 'main .help-btn',
    title: 'Help on every section',
    text: 'The ? next to a heading explains the section, its controls and who can use it.',
  },
  {
    id: 'nav-deployments',
    target: '[data-nav="deployments"]',
    title: 'Deployments',
    text: 'Every deployment: trigger, progress, duration. Click one for its build log.',
  },
  {
    id: 'nav-events',
    target: '[data-nav="events"]',
    title: 'Events',
    text: 'The live audit log: who did what.',
  },
  {
    id: 'nav-account',
    target: '[data-nav="account"]',
    title: 'Account',
    text: 'Password and theme. Admins also manage users, tokens and backups.',
  },
  {
    id: 'conn',
    target: '#conn',
    title: 'Live connection',
    text: 'Live: changes arrive without reloading. Reconnecting: the connection dropped and is being restored.',
  },
  {
    id: 'theme',
    target: '[data-tour="theme"]',
    title: 'Light or dark',
    text: 'Light or dark. Press t anywhere.',
  },
  {
    id: 'shortcuts',
    target: '[data-tour="help"]',
    title: 'Help and shortcuts',
    text: 'Keyboard shortcuts (or press ?), the guide link and this tour.',
  },
];
