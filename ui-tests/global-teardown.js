const fs = require('fs');
const path = require('path');

module.exports = async () => {
  const pidFile = path.join(__dirname, '.work', 'daemon.pid');
  if (!fs.existsSync(pidFile)) return;
  const pid = Number(fs.readFileSync(pidFile, 'utf8'));
  try {
    // SIGTERM: the daemon stops the apps it supervises before exiting.
    process.kill(pid, 'SIGTERM');
  } catch {
    return;
  }
  for (let i = 0; i < 100; i += 1) {
    try {
      process.kill(pid, 0);
    } catch {
      return;
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  process.kill(pid, 'SIGKILL');
};
