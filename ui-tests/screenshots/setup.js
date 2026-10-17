// Starts a daemon for the dashboard screenshots (npm run screenshots): like
// ../global-setup.js, but in its own scratch directory, with the built-in
// proxy on unprivileged ports (for a domain and several instances) and the
// fake GitHub releases API (for the update banner). The demo data itself is
// created by dashboard.shots.js through the API and the browser.
const { execFileSync, spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ROOT = path.resolve(__dirname, '..', '..');
// A neutral path: it shows up in the screenshots as the apps' source.
const WORK = process.env.SHOTS_WORK || path.join(os.tmpdir(), 'aegis-demo');
const GIT_ENV = {
  GIT_AUTHOR_NAME: 'Demo',
  GIT_AUTHOR_EMAIL: 'demo@example.com',
  GIT_COMMITTER_NAME: 'Demo',
  GIT_COMMITTER_EMAIL: 'demo@example.com',
};

function git(cwd, ...args) {
  execFileSync('git', args, { cwd, stdio: 'pipe', env: { ...process.env, ...GIT_ENV } });
}

async function waitFor(url, ms) {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`${url} did not come up; see ${path.join(WORK, 'daemon.log')}`);
}

function dockerWorks() {
  try {
    execFileSync('docker', ['info'], { stdio: 'ignore', timeout: 20_000 });
    return true;
  } catch {
    return false;
  }
}

module.exports = async (config) => {
  const baseURL = config.projects[0].use.baseURL;
  const webPort = Number(new URL(baseURL).port);
  const binDir = path.resolve(ROOT, process.env.BIN_DIR || 'target/debug');
  const exe = process.platform === 'win32' ? 'aegis-daemon.exe' : 'aegis-daemon';
  const daemon = path.join(binDir, exe);
  if (!fs.existsSync(daemon)) throw new Error(`${daemon} not found; run cargo build -p aegis-daemon (or set BIN_DIR)`);

  fs.rmSync(WORK, { recursive: true, force: true });
  fs.mkdirSync(WORK, { recursive: true });

  // "shop": examples/node-app in a git repository, with a version field.
  const repo = path.join(WORK, 'shop');
  fs.cpSync(path.join(ROOT, 'examples', 'node-app'), repo, { recursive: true });
  const index = path.join(repo, 'index.js');
  // Log each request, so the Logs tab has something to show.
  fs.writeFileSync(index, fs.readFileSync(index, 'utf8')
    .replace("status: 'ok'", "status: 'ok', version: 'v1'")
    .replace('const server = http.createServer((req, res) => {', "const server = http.createServer((req, res) => {\n  console.log(`${req.method} ${req.url} 200`);\n  if (req.url === '/slow') console.error('warning: slow request took 1200 ms');"));
  git(repo, 'init', '-q', '-b', 'main');
  git(repo, 'add', '.');
  git(repo, 'commit', '-q', '-m', 'First version of the shop');

  // "worker": examples/python-app as a directory on the server.
  const worker = path.join(WORK, 'worker');
  fs.cpSync(path.join(ROOT, 'examples', 'python-app'), worker, { recursive: true });

  // "web": examples/docker-app, when Docker is usable here.
  let web = null;
  if (dockerWorks()) {
    web = path.join(WORK, 'web');
    fs.cpSync(path.join(ROOT, 'examples', 'docker-app'), web, { recursive: true });
  }

  const proxyHttp = webPort + 40;
  const fwd = (p) => p.replace(/\\/g, '/');
  const configPath = path.join(WORK, 'daemon.toml');
  fs.writeFileSync(configPath, `[daemon]
host = "127.0.0.1"
port = ${webPort + 1}
database_path = "${fwd(WORK)}/aegis.db"
data_dir = "${fwd(WORK)}/data"
log_level = "info"

[web]
enabled = true
host = "127.0.0.1"
port = ${webPort}

[proxy]
enabled = true
http_port = ${proxyHttp}
https_port = ${proxyHttp + 1}

[update]
check = true                # against the fake API below, never GitHub
`);

  const githubPort = webPort + 2;
  const fake = spawn(process.execPath, [path.join(__dirname, '..', 'fake-github.js'), String(githubPort)], {
    stdio: 'ignore',
    detached: true,
  });
  fs.writeFileSync(path.join(WORK, 'fake-github.pid'), String(fake.pid));
  fake.unref();
  await waitFor(`http://127.0.0.1:${githubPort}/repos/shivam411/Aegis/releases/latest`, 10_000);

  const log = fs.openSync(path.join(WORK, 'daemon.log'), 'a');
  const child = spawn(daemon, ['run'], {
    cwd: WORK,
    env: { ...process.env, AEGIS_CONFIG: configPath, AEGIS_UPDATE_API_URL: `http://127.0.0.1:${githubPort}` },
    stdio: ['ignore', log, log],
    detached: true,
  });
  fs.writeFileSync(path.join(WORK, 'daemon.pid'), String(child.pid));
  child.unref();

  await waitFor(`${baseURL}/healthz`, 120_000);
  const password = fs.readFileSync(path.join(WORK, 'data', 'initial-admin-password'), 'utf8').trim();
  fs.writeFileSync(path.join(__dirname, '.env.json'), JSON.stringify({
    work: WORK, repo, worker, web, password, baseURL, proxyHttp, gitEnv: GIT_ENV,
  }));
};
