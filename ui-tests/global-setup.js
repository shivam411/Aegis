// Starts aegis-daemon with the web dashboard on a scratch directory and
// prepares a git repository holding examples/node-app for the tests to deploy.
const { execFileSync, spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const WORK = path.join(__dirname, '.work');

function git(cwd, ...args) {
  execFileSync('git', args, {
    cwd,
    stdio: 'pipe',
    env: {
      ...process.env,
      GIT_AUTHOR_NAME: 'e2e',
      GIT_AUTHOR_EMAIL: 'e2e@example.com',
      GIT_COMMITTER_NAME: 'e2e',
      GIT_COMMITTER_EMAIL: 'e2e@example.com',
    },
  });
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

module.exports = async (config) => {
  const baseURL = config.projects[0].use.baseURL;
  const webPort = new URL(baseURL).port;
  const grpcPort = Number(webPort) + 1;
  const binDir = path.resolve(ROOT, process.env.BIN_DIR || 'target/debug');
  const daemon = path.join(binDir, process.platform === 'win32' ? 'aegis-daemon.exe' : 'aegis-daemon');
  if (!fs.existsSync(daemon)) {
    throw new Error(`${daemon} not found; run cargo build -p aegis-daemon (or set BIN_DIR)`);
  }

  fs.rmSync(WORK, { recursive: true, force: true });
  fs.mkdirSync(WORK, { recursive: true });

  // The app: examples/node-app in a git repository, tagged with a version.
  const repo = path.join(WORK, 'repo');
  fs.cpSync(path.join(ROOT, 'examples', 'node-app'), repo, { recursive: true });
  const index = path.join(repo, 'index.js');
  fs.writeFileSync(index, fs.readFileSync(index, 'utf8').replace("status: 'ok'", "status: 'ok', version: 'v1'"));
  git(repo, 'init', '-q', '-b', 'main');
  git(repo, 'add', '.');
  git(repo, 'commit', '-q', '-m', 'v1');

  // A second app for the resource tests: /spin starts a busy loop and /hog
  // allocates 200 MiB.
  const hog = path.join(WORK, 'hog');
  fs.mkdirSync(hog);
  fs.writeFileSync(path.join(hog, 'requirements.txt'), '');
  fs.writeFileSync(path.join(hog, 'app.py'), `import http.server, os, threading
hog = []
def spin():
    while True:
        pass
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/spin":
            threading.Thread(target=spin, daemon=True).start()
        if self.path == "/hog":
            for _ in range(50):
                hog.append(bytearray(4 * 1024 * 1024))
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"ok")
    def log_message(self, *a):
        pass
print("hog listening on", os.environ["PORT"], flush=True)
http.server.ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
`);

  // An app with an env file on the server, as left by PM2 (envfile.spec.js).
  const envApp = path.join(WORK, 'envapp');
  fs.mkdirSync(envApp);
  fs.writeFileSync(path.join(envApp, 'index.js'), 'console.log("env app");\n');
  const envDir = path.join(WORK, 'envfiles');
  fs.mkdirSync(envDir);
  const envFile = path.join(envDir, 'envapp.env');
  fs.writeFileSync(envFile, [
    '# server-side settings',
    'MONGODB_URI=mongodb+srv://app:e2e-s3ntinel@cluster0.example.net/img',
    'NEXT_PUBLIC_SITE_URL=https://img.example.com',
    'PORT=3000',
    '',
  ].join('\n'));

  // A folder of existing checkouts, as on a server that ran them with PM2
  // (sources.spec.js scans and imports it): a Next.js app with a hard-coded
  // port in a checkout of a GitHub repository (git metadata written by
  // hand, read without git), and a plain Python folder.
  const scanDir = path.join(WORK, 'scan');
  const imgMan = path.join(scanDir, 'img-man');
  fs.mkdirSync(path.join(imgMan, '.git'), { recursive: true });
  fs.writeFileSync(path.join(imgMan, 'package.json'), JSON.stringify({
    name: 'img-man',
    scripts: { build: 'next build', start: 'next start -p 4000' },
  }));
  fs.writeFileSync(path.join(imgMan, '.git', 'HEAD'), 'ref: refs/heads/main\n');
  fs.writeFileSync(path.join(imgMan, '.git', 'config'),
    '[core]\n\tbare = false\n[remote "origin"]\n\turl = git@github.com:acme/img-man.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n');
  fs.writeFileSync(path.join(imgMan, '.env'), 'MONGODB_URI=mongodb://u:e2e-s3ntinel@db/img\n');
  fs.mkdirSync(path.join(imgMan, 'node_modules', 'next'), { recursive: true });
  fs.writeFileSync(path.join(imgMan, 'node_modules', 'next', 'package.json'), '{}');
  const notes = path.join(scanDir, 'notes-api');
  fs.mkdirSync(notes, { recursive: true });
  fs.writeFileSync(path.join(notes, 'requirements.txt'), '');
  fs.writeFileSync(path.join(notes, 'app.py'), 'print("notes")\n');

  const configPath = path.join(WORK, 'daemon.toml');
  fs.writeFileSync(configPath, `[daemon]
host = "127.0.0.1"
port = ${grpcPort}
database_path = '${WORK}/aegis.db'
data_dir = '${WORK}/data'
log_level = "info"

[web]
enabled = true
host = "127.0.0.1"
port = ${webPort}

[update]
check = true                # against the fake API below, never GitHub

[apps]
env_file_dirs = ['${envDir}']
import_dirs = ['${scanDir}']
`);

  // A fake GitHub releases API announcing v0.99.0 (no network access).
  const githubPort = Number(webPort) + 2;
  const fake = spawn(process.execPath, [path.join(__dirname, 'fake-github.js'), String(githubPort)], {
    stdio: 'ignore',
    detached: true,
  });
  fs.writeFileSync(path.join(WORK, 'fake-github.pid'), String(fake.pid));
  fake.unref();
  await waitFor(`http://127.0.0.1:${githubPort}/repos/shivam411/Aegis/releases/latest`, 10_000);

  const log = fs.openSync(path.join(WORK, 'daemon.log'), 'a');
  const child = spawn(daemon, ['run'], {
    cwd: WORK,
    env: {
      ...process.env,
      AEGIS_CONFIG: configPath,
      AEGIS_UPDATE_API_URL: `http://127.0.0.1:${githubPort}`,
    },
    stdio: ['ignore', log, log],
    detached: true,
  });
  fs.writeFileSync(path.join(WORK, 'daemon.pid'), String(child.pid));
  child.unref();

  await waitFor(`${baseURL}/healthz`, 120_000);
  const password = fs.readFileSync(path.join(WORK, 'data', 'initial-admin-password'), 'utf8').trim();
  fs.writeFileSync(path.join(WORK, 'env.json'), JSON.stringify({ repo, hog, envApp, envFile, scanDir, password, baseURL }));
};
