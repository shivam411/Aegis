// A stand-in for the GitHub releases API, so the dashboard's update banner
// can be tested without network access. global-setup.js starts it and points
// the daemon at it with AEGIS_UPDATE_API_URL (accepted for loopback only).
// It publishes no downloadable assets, so an upgrade could never install.
const http = require('http');

const port = Number(process.argv[2]);
const release = {
  tag_name: 'v0.99.0',
  name: 'Aegis 0.99.0',
  html_url: 'https://github.com/shivam411/Aegis/releases/tag/v0.99.0',
  published_at: '2026-10-01T00:00:00Z',
  draft: false,
  prerelease: false,
  assets: [],
};

http
  .createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/repos/shivam411/Aegis/releases/latest') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(release));
      return;
    }
    res.writeHead(404, { 'Content-Type': 'application/json' });
    res.end('{"message":"Not Found"}');
  })
  .listen(port, '127.0.0.1');
