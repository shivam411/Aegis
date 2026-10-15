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
  const daemon = path.join(binDir, 'aegis-daemon');
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

  const configPath = path.join(WORK, 'daemon.toml');
  fs.writeFileSync(configPath, `[daemon]
host = "127.0.0.1"
port = ${grpcPort}
database_path = "${WORK}/aegis.db"
data_dir = "${WORK}/data"
log_level = "info"

[web]
enabled = true
host = "127.0.0.1"
port = ${webPort}
`);

  const log = fs.openSync(path.join(WORK, 'daemon.log'), 'a');
  const child = spawn(daemon, ['run'], {
    cwd: WORK,
    env: { ...process.env, AEGIS_CONFIG: configPath },
    stdio: ['ignore', log, log],
    detached: true,
  });
  fs.writeFileSync(path.join(WORK, 'daemon.pid'), String(child.pid));
  child.unref();

  await waitFor(`${baseURL}/healthz`, 30_000);
  const password = fs.readFileSync(path.join(WORK, 'data', 'initial-admin-password'), 'utf8').trim();
  fs.writeFileSync(path.join(WORK, 'env.json'), JSON.stringify({ repo, hog, password, baseURL }));
};
