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
    summary: 'The home page: how busy this server is, and every app Aegis runs on it with its current state. It updates live.',
    controls: [
      ['+ New app', 'Opens the new app wizard (admins).'],
      ['App cards', 'One card per app; click its name to open the app page.'],
    ],
    roles: 'Everyone can see it. Deploy, Start, Stop and Restart need the deployer role; + New app needs admin.',
    docs: 'overview.md',
  },
  'overview.host': {
    title: 'Host resources',
    summary: 'The server itself, refreshed every 5 seconds: CPU use, memory, free disk space for the data directory, and load average. The bar turns amber at 75% and red at 90%.',
    controls: [
      ['CPU', 'Percent of all cores in use, with the core count and host name.'],
      ['Memory', 'Used of total memory, plus swap if the server has any.'],
      ['Disk', 'How full the disk holding Aegis data is, and what is still free.'],
      ['Load', 'The 1, 5 and 15 minute load averages, and how long the server has been up.'],
    ],
    roles: 'Everyone.',
    docs: 'overview.md#host-tiles',
  },
  'overview.apps': {
    title: 'Apps',
    summary: 'Every app with its status, live version, uptime, restart count and current CPU and memory use (against its limits, if set). A blue Deploying pill shows a deployment in progress.',
    controls: [
      ['Deploy', 'Starts a deployment of the latest code with the app\'s default strategy and opens its progress.'],
      ['Start / Stop / Restart', 'Controls the running process. Stop and Restart ask first.'],
      ['Logs', 'Opens the app\'s live log.'],
    ],
    roles: 'Everyone can see the cards. Deploy, Start, Stop and Restart need deployer or admin.',
    docs: 'overview.md#app-cards',
  },
  wizard: {
    title: 'New app',
    summary: 'Adds an app in two steps. First say where the code is; Aegis then looks at it and suggests install, build, test and start commands, a port and a health check. Review them, then create the app. Or scan a folder on the server and import several existing apps at once.',
    controls: [
      ['Git repository / Directory on this server', 'Where the code comes from. A repository is cloned on every deploy; a directory is copied.'],
      ['Scan a folder on the server', 'Looks for apps in a folder allowed by the daemon configuration ([apps] import_dirs or env_file_dirs) and imports the ones you select, without deploying them.'],
      ['App folder in the source', 'Optional, for monorepos: only this folder (e.g. apps/web) is built and run.'],
      ['Private repository', 'If Aegis can\'t read a GitHub repository, it offers to create the app and set up a deploy key first, then detect the settings with it.'],
      ['Detect settings', 'Inspects the code and fills in step 2. A start script that fixes its port (e.g. next start -p 4000) is changed to read $PORT (npx next start -p $PORT).'],
      ['Deploy right away', 'Starts the first deployment as soon as the app is created.'],
    ],
    roles: 'Admins only.',
    docs: 'new-app.md',
  },
  'wizard.scan': {
    title: 'Scan results',
    summary: 'Apps found in the folder, with the settings an import would save. Nothing is changed until you import, and imported apps are not deployed. Apps already registered (same repository, branch and folder, or same directory) are listed but can\'t be selected.',
    controls: [
      ['Name / Start command / Port', 'Edit before importing. A hard-coded port in the start script is already replaced with $PORT.'],
      ['Source', 'Clone the repository (the checkout\'s origin, at its current branch) or copy this folder at every deployment.'],
      ['Check', 'For GitHub repositories: asks GitHub, without credentials, whether the repository is public. Private ones need a deploy key (Settings → Repository after import).'],
      ['Env file', 'Use the app\'s .env as its Env file (only if its folder is in [apps] env_file_dirs). Only variable names are shown, never values.'],
      ['Warnings', 'Port in use (probably the current deployment: stop it before the first deploy, or map a domain), deploy key needed, .env not readable by the daemon (with the chgrp/chmod fix), hard-coded port.'],
      ['Import selected', 'Registers the selected apps and lists the next steps for each.'],
    ],
    roles: 'Admins only.',
    docs: 'new-app.md#scan-a-folder-on-the-server',
  },

  // --- App page -----------------------------------------------------------
  app: {
    title: 'App page',
    summary: 'Everything about one app. The header shows its status, live version, uptime, restarts and source; the tabs below switch between deploying, releases, logs, resources, settings and activity.',
    controls: [
      ['Start / Stop / Restart', 'Controls the app\'s process (or container). Stop and Restart ask for confirmation.'],
      ['Tabs', 'Deploy, Releases, Logs, Resources, Settings, Activity. Keys 1 to 6 switch between them.'],
    ],
    roles: 'Everyone can look. Process control, deploying and rolling back need deployer; settings need admin.',
    docs: 'app.md',
  },
  'app.deploy': {
    title: 'New deployment',
    summary: 'Builds and releases a new version. A git app can deploy any branch or commit; a directory app copies the directory as it is now.',
    controls: [
      ['Branch', 'Which branch to build (git apps). Blank uses the app\'s branch.'],
      ['Commit', 'A commit SHA on that branch. Blank uses its latest commit.'],
      ['Strategy', 'How the new release replaces the live one: Graceful switch, Immediate, Rolling or Blue-green.'],
      ['Deploy', 'Queues the deployment; its pipeline appears below and updates live.'],
    ],
    roles: 'Deployer or admin. Viewers see the form without the Deploy button.',
    docs: 'deploy.md#new-deployment',
  },
  'app.pipeline': {
    title: 'Deployment progress',
    summary: 'The latest deployment of this app as it runs: seven stages from Clone to Promote, how long each took, rollout steps for several instances, the outcome and the full build log.',
    controls: [
      ['Stages', 'Clone, Install, Build, Test, Package, Verify (health check) and Promote (go live). A dash means skipped.'],
      ['Build log', 'All output of the build commands; open by default while running or after a failure.'],
      ['Details', 'Opens the deployment page with its full event list.'],
    ],
    roles: 'Everyone.',
    docs: 'deploy.md#deployment-progress',
  },
  'app.instances': {
    title: 'Instances',
    summary: 'Shown when the app runs more than one copy. Each instance has its own process, port and log; the built-in proxy spreads requests over the running ones. After a Blue-green deployment, the previous release can stay running for a while so rolling back is instant.',
    controls: [
      ['Logs', 'Opens the log of that one instance.'],
    ],
    roles: 'Everyone.',
    docs: 'docker-and-instances.md#instances',
  },
  'app.releases': {
    title: 'Releases',
    summary: 'Every build of this app that Aegis still keeps, newest first. Live is the release serving traffic. Older releases can be rolled back to without rebuilding.',
    controls: [
      ['Roll back', 'Switches the app to that release after it passes the health check. The current release stays available.'],
    ],
    roles: 'Everyone can see releases; rolling back needs deployer or admin.',
    docs: 'releases.md',
  },
  'app.logs': {
    title: 'Logs',
    summary: 'The app\'s output, live. The last 500 lines are loaded first and new lines stream in as they are written. Supervisor lines (starts, stops, crashes) are shown in blue, stderr in red.',
    controls: [
      ['Search', 'Highlights matching lines and hides the rest. Press / to jump here.'],
      ['Stream', 'Show all output, only stdout, only stderr, or only supervisor messages.'],
      ['Follow', 'Keeps the view scrolled to the newest line. Scrolling up turns it off.'],
      ['Pause / Resume', 'Freezes the view; lines that arrive meanwhile are counted and added on Resume.'],
      ['Clear / Download', 'Empties the view, or saves the loaded lines as a .log file.'],
      ['Instance', 'With several instances, picks whose log to show.'],
    ],
    roles: 'Everyone.',
    docs: 'logs.md',
  },
  'app.resources': {
    title: 'Live usage',
    summary: 'What the app uses right now, updated every few seconds: CPU in cores, memory, how much of the time its CPU limit held it back (throttled), threads and processes, open files, and how often it ran out of memory (OOM kills).',
    controls: [],
    roles: 'Everyone.',
    docs: 'resources.md#live-usage',
  },
  'app.usage': {
    title: 'Usage charts',
    summary: 'CPU and memory over time. The solid line is the average, the faint line the peak, and the dashed red line the limit while one was set. Hover a chart to read a point.',
    controls: [
      ['Live · 5 min / 1 hour / 24 hours / 7 days', 'The time range. Live keeps adding new samples.'],
    ],
    roles: 'Everyone.',
    docs: 'resources.md#usage-charts',
  },
  'app.limits': {
    title: 'Limits',
    summary: 'Caps on CPU, memory and processes for this app, applied to the running app at once without a restart, and again on every start and deploy. With several instances each one gets the limit on its own.',
    controls: [
      ['CPU', 'Maximum cores the app may use. Tick No limit to remove it.'],
      ['Memory', 'Maximum memory in MiB. Above it the app is stopped and restarted; at 90% it is asked to reclaim memory first.'],
      ['Processes', 'Maximum processes and threads (where the server supports it).'],
    ],
    roles: 'Admins change limits; everyone else sees them read-only.',
    docs: 'resources.md#limits',
  },
  'app.settings': {
    title: 'Build & run',
    summary: 'How the app is built and started. Blank fields fall back to the app\'s aegis.toml or to auto-detection. Saving shows the change to aegis.toml first; it applies to the next deployment.',
    controls: [
      ['Install / Build / Test / Start command', 'Shell commands run during the pipeline and to start the app.'],
      ['Port / Health check URL', 'Where the app listens, and the URL that must answer (below 500) before a release goes live.'],
      ['No HTTP health check', 'Treat the release as healthy while its process stays up.'],
      ['Default strategy / Instances', 'The strategy used when a deployment does not pick one, and how many copies to run (more than one needs a domain).'],
      ['Environment variables', 'Plain settings written into each release. Use Secrets for passwords and keys.'],
      ['View aegis.toml / Show detected defaults', 'Shows the file the next release gets, or what auto-detection would choose.'],
    ],
    roles: 'Admins. Others can only view aegis.toml.',
    docs: 'settings.md#build-and-run',
  },
  'app.domains': {
    title: 'Domains',
    summary: 'Serve the app on your own domain names through Aegis\'s built-in proxy, with automatic HTTPS from Let\'s Encrypt or certificate files. Point each domain\'s DNS at this server first.',
    controls: [
      ['Domain / TLS', 'The host name, and whether it gets automatic HTTPS, HTTPS from your own certificate files, or HTTP only.'],
      ['Remove', 'Stops serving the app on that domain right away.'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#domains',
  },
  'app.repository': {
    title: 'Repository',
    summary: 'Where deployments get the code, and the app\'s SSH deploy key for private repositories. Aegis generates an Ed25519 key pair; the private key stays on this server, encrypted with the secrets key, and is only written to a temporary file (readable by Aegis alone) while git runs. Only the public key is ever shown.',
    controls: [
      ['Generate deploy key', 'Creates the key pair. Add the public key to the repository\'s deploy keys (on GitHub: Settings → Deploy keys → Add deploy key) and leave "Allow write access" unticked.'],
      ['Copy', 'Copies the public key line (ssh-ed25519 …).'],
      ['Test access', 'Runs git ls-remote with the key and shows the result, or git\'s error with what to do about it.'],
      ['Generate a new key / Delete key', 'Replaces or removes the key; also update the repository\'s deploy keys.'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#repository',
  },
  'app.env': {
    title: 'Environment',
    summary: 'Where each environment variable comes from, lowest to highest: the env file on the server, [env] in aegis.toml, Variables, Secrets, then PORT (always set by Aegis). Build commands get the env file, [env] and Variables, never Secrets. Only names are shown, never values.',
    controls: [
      ['Env file path', 'An absolute path on the server, such as the app\'s existing .env. It is read at every build and every start (edit the file, then restart), never stored. It must be in a directory listed in the daemon configuration ([apps] env_file_dirs), and the daemon\'s user must be able to read it (e.g. chgrp aegis FILE && chmod 640 FILE).'],
      ['Status', 'Whether the file could be read just now, and why not (missing, permission denied, outside the allowed directories, a syntax error on a given line).'],
      ['Value from / Overrides / Build', 'Which source wins for each name, which lower sources it overrides, and whether build commands see it.'],
      ['Import .env', 'Paste a .env or choose one (read in the browser), preview the names with a suggested type, then save each as a Variable or a Secret, or skip it. Multi-line values can only be Secrets.'],
    ],
    roles: 'Everyone sees the names and sources; admins set the env file and import.',
    docs: 'settings.md#environment',
  },
  'app.secrets': {
    title: 'Secrets',
    summary: 'Environment variables stored encrypted on the server and passed to the app when it starts. Values can be replaced but never read back. A running app picks up a change when it restarts. Secrets are not available to the build command: anything the build needs (such as NEXT_PUBLIC_* variables) goes in Variables or the env file.',
    controls: [
      ['Value', 'Saved exactly as typed or pasted, including line breaks, so a PEM private key or certificate can be pasted as it is. The field is masked; Show reveals it while you edit (where the browser supports masking).'],
      ['Save secret', 'Adds or replaces a secret, then offers to restart the app.'],
      ['Replace / Delete', 'Fills the form to set a new value, or removes the secret.'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#secrets',
  },
  'app.schedule': {
    title: 'Daily deploy',
    summary: 'Deploys the app automatically once a day at a set time (UTC), for example to pick up the latest commit every night.',
    controls: [
      ['Time (UTC) / Branch', 'When to deploy, and which branch (git apps).'],
    ],
    roles: 'Admins.',
    docs: 'settings.md#daily-deploy',
  },
  'app.webhook': {
    title: 'GitHub webhook',
    summary: 'Deploys on every push to the app\'s branch. Generate a secret here, then add a webhook in the GitHub repository with the payload URL and secret shown.',
    controls: [
      ['Generate / Rotate secret', 'Creates the shared secret. It is shown once; rotating breaks the old one.'],
      ['Send test delivery', 'Checks that the webhook endpoint accepts a signed request.'],
      ['Recent deliveries', 'What GitHub sent lately and what Aegis did with it.'],
    ],
    roles: 'Admins. Needs a git repository as the source.',
    docs: 'settings.md#github-webhook',
  },
  'app.about': {
    title: 'About',
    summary: 'Fixed facts about the app: its id (used by the CLI and API), detected runtime, whether it runs as a process or a Docker container, when it was created and where its code comes from.',
    controls: [],
    roles: 'Admins.',
    docs: 'settings.md#about',
  },
  'app.activity': {
    title: 'Activity',
    summary: 'This app\'s own audit log: deployments, build stages, releases, rollbacks, process starts and crashes, setting and limit changes, newest first and live. Open Details on an entry for its full data.',
    controls: [],
    roles: 'Everyone.',
    docs: 'activity.md',
  },

  // --- Deployments ----------------------------------------------------------
  deployments: {
    title: 'Deployments',
    summary: 'Every deployment of every app, newest first: its status, release, what triggered it (dashboard, CLI, webhook, schedule), the stage it reached and how long it took. Click a row for its pipeline, build log and events.',
    controls: [
      ['App / Status filters', 'Narrow the list to one app or to in-progress, succeeded, failed or rolled-back deployments.'],
    ],
    roles: 'Everyone.',
    docs: 'deployments.md',
  },
  deployment: {
    title: 'Deployment details',
    summary: 'One deployment: its pipeline stages, rollout steps, outcome, build log, and every event it produced in order.',
    controls: [
      ['Open app', 'Goes to the app page.'],
    ],
    roles: 'Everyone.',
    docs: 'deployments.md#deployment-details',
  },

  // --- Events ---------------------------------------------------------------
  events: {
    title: 'Events',
    summary: 'The audit log for the whole server: every deployment, process change, sign-in, token, setting, backup and update, with who did it. New events appear at the top while Live is ticked.',
    controls: [
      ['App / Category / Last N', 'Filter by app and kind of event, and choose how many to load.'],
      ['Filter by text', 'Matches the event type and its data. Press / to jump here.'],
      ['Live', 'Untick to stop new events being added while you read.'],
      ['Details', 'Shows an event\'s full data as JSON.'],
    ],
    roles: 'Everyone.',
    docs: 'events.md',
  },

  // --- Account --------------------------------------------------------------
  account: {
    title: 'Account',
    summary: 'Your password and display preferences. Admins also manage users, API tokens and backups here.',
    controls: [
      ['Sign out', 'Ends this browser session.'],
    ],
    roles: 'Everyone sees Password and Appearance; Users, API tokens and Backups are for admins.',
    docs: 'account.md',
  },
  'account.users': {
    title: 'Users',
    summary: 'Everyone signs in with their own account, so the audit log shows who did what. New users and password resets get a random password that is shown once for you to pass on.',
    controls: [
      ['Role', 'Viewer reads only; deployer can also deploy, roll back, start and stop; admin can do everything.'],
      ['Reset password', 'Signs the user out everywhere and shows a new password once.'],
      ['Delete', 'Removes the user. Their past actions stay in the audit log. You cannot delete yourself.'],
      ['Add user', 'Creates a user with the chosen role.'],
    ],
    roles: 'Admins.',
    docs: 'account.md#users',
  },
  'account.tokens': {
    title: 'API tokens',
    summary: 'Tokens for the CLI, scripts and CI, sent as an Authorization: Bearer header. The token is shown once when created.',
    controls: [
      ['Scope', 'deploy can read and act (deploy, roll back, start, stop); read can only look.'],
      ['Revoke', 'Anything using the token loses access immediately.'],
    ],
    roles: 'Admins.',
    docs: 'account.md#api-tokens',
  },
  'account.backups': {
    title: 'Backups',
    summary: 'Archives of the database, the secrets key, certificates and optionally the release directories. Restoring is done on the server with the daemon stopped; the exact commands are shown below the list.',
    controls: [
      ['Include releases', 'Also archive the built releases (can be large).'],
      ['Back up now', 'Creates a backup on the server.'],
      ['Download / Delete', 'Saves an archive to your computer, or removes it from the server.'],
    ],
    roles: 'Admins. Backups contain password hashes and the secrets key: keep downloads safe.',
    docs: 'account.md#backups',
  },
  'account.password': {
    title: 'Password',
    summary: 'Change your own password (at least 12 characters). Every session of yours is signed out, including this one.',
    controls: [],
    roles: 'Everyone.',
    docs: 'account.md#password',
  },
  'account.appearance': {
    title: 'Appearance & shortcuts',
    summary: 'Light, dark or system theme (remembered in this browser), the keyboard shortcuts, and the guided tour.',
    controls: [
      ['Theme', 'Match the system, or always light or dark. Press t to toggle.'],
      ['Take the tour again', 'Replays the first-run tour.'],
    ],
    roles: 'Everyone.',
    docs: 'account.md#appearance-and-shortcuts',
  },

  // --- Shell ----------------------------------------------------------------
  update: {
    title: 'Aegis updates',
    summary: 'Aegis checks GitHub for new releases and says so here. Upgrading backs up first, verifies the download, swaps the binaries and restarts; apps restart with it, so expect a short outage.',
    controls: [
      ['Upgrade now', 'Starts the update after a confirmation (admins, where this server can update itself).'],
      ['Release notes', 'What changed in the new version.'],
      ['Later', 'Hides the notice for this version in this browser.'],
    ],
    roles: 'Everyone sees the notice; only admins can upgrade.',
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
    text: 'This short tour shows where things are. It takes about a minute; skip it now and replay it any time from the ? button at the top or from Account.',
  },
  {
    id: 'nav-overview',
    target: '[data-nav="overview"]',
    title: 'Overview',
    text: 'Your home page: the server\'s health and every app with its status, version and uptime.',
  },
  {
    id: 'host',
    route: '/',
    target: '#host',
    title: 'Server health',
    text: 'CPU, memory, disk and load of this server, refreshed every few seconds. Bars turn amber and red as they fill up.',
  },
  {
    id: 'new-app',
    route: '/',
    role: 'admin',
    target: '[data-tour="new-app"]',
    title: 'Add an app',
    text: 'Point Aegis at a git repository or a directory on this server. It detects how to build and run the app, and can deploy it straight away.',
  },
  {
    id: 'deploy',
    route: '/',
    role: 'deployer',
    needsApp: true,
    target: '[data-app] [data-action="deploy"]',
    title: 'Deploy',
    text: 'Builds the latest code, health-checks it and switches traffic over. If the new release fails its health check, the old one keeps running.',
  },
  {
    id: 'tabs',
    route: 'app',
    needsApp: true,
    target: '.tabs',
    title: 'Inside an app',
    text: 'Each app has tabs: Deploy, Releases (roll back), Logs, Resources (usage and limits), Settings and Activity. Keys 1 to 6 switch tabs.',
  },
  {
    id: 'help',
    target: 'main .help-btn',
    title: 'Help on every section',
    text: 'This small ? next to a heading explains what the section shows, what each control does and who can use it, with a link to the full guide.',
  },
  {
    id: 'nav-deployments',
    target: '[data-nav="deployments"]',
    title: 'Deployments',
    text: 'Every deployment of every app: what started it, how far it got and how long it took. Click one for its build log.',
  },
  {
    id: 'nav-events',
    target: '[data-nav="events"]',
    title: 'Events',
    text: 'The audit log: deploys, restarts, crashes, sign-ins, setting changes and more, with who did what. It updates live.',
  },
  {
    id: 'nav-account',
    target: '[data-nav="account"]',
    title: 'Account',
    text: 'Change your password and theme. Admins also manage users, API tokens and backups here.',
  },
  {
    id: 'conn',
    target: '#conn',
    title: 'Live connection',
    text: 'Live means the page receives changes as they happen; no need to reload. Reconnecting means the connection dropped and is being restored.',
  },
  {
    id: 'theme',
    target: '[data-tour="theme"]',
    title: 'Light or dark',
    text: 'Switches between the light and dark theme. Press t anywhere for the same.',
  },
  {
    id: 'shortcuts',
    target: '[data-tour="help"]',
    title: 'Help and shortcuts',
    text: 'Opens the keyboard shortcuts (the ? key does too), a link to the dashboard guide, and this tour. That\'s it: you are all set.',
  },
];
