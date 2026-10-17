const fs = require('fs');
const path = require('path');

function stop(file) {
  if (!fs.existsSync(file)) return null;
  const pid = Number(fs.readFileSync(file, 'utf8'));
  try {
    process.kill(pid, 'SIGTERM');
    return pid;
  } catch {
    return null;
  }
}

module.exports = async () => {
  const envFile = path.join(__dirname, '.env.json');
  if (!fs.existsSync(envFile)) return;
  const { work } = JSON.parse(fs.readFileSync(envFile, 'utf8'));
  stop(path.join(work, 'fake-github.pid'));
  const pid = stop(path.join(work, 'daemon.pid'));
  if (!pid) return;
  // SIGTERM: the daemon stops the apps it supervises before exiting.
  for (let i = 0; i < 100; i += 1) {
    try {
      process.kill(pid, 0);
    } catch {
      return;
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  try {
    process.kill(pid, 'SIGKILL');
  } catch {
    /* gone */
  }
};
