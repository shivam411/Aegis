// Seeds demo data and captures the screenshots used by docs/dashboard/.
// Run with `npm run screenshots` (see ../playwright.screenshots.config.js).
// Everything shown is demo data: apps from examples/, users "alice" and
// "bob", tokens "ci-deploy" and "monitoring", host name "demo-server".
const { test, expect, devices } = require('@playwright/test');
const { execFileSync } = require('child_process');
const fs = require('fs');
const http = require('http');
const path = require('path');

const OUT = path.resolve(__dirname, '..', '..', 'docs', 'images', 'dashboard');
const SHOP_PORT = 18760;
const MIB = 1024 * 1024;

function env() {
  return JSON.parse(fs.readFileSync(path.join(__dirname, '.env.json'), 'utf8'));
}

test.describe.configure({ mode: 'serial' });

/** Hides the machine's real host name, and the update notice once shown. */
async function prepare(context, { dismissUpdate = false, tourDone = true } = {}) {
  await context.route('**/api/v1/host', async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    body.hostname = 'demo-server';
    await route.fulfill({ response: res, json: body });
  });
  await context.addInitScript(({ dismissUpdate: d, tourDone: t }) => {
    try {
      if (t) {
        for (const user of ['admin', 'alice', 'bob']) localStorage.setItem(`aegis.tour.${user}`, 'done');
      }
      if (d) localStorage.setItem('aegis-dismissed-available-v0.99.0', '1');
    } catch {
      /* ignore */
    }
  }, { dismissUpdate, tourDone });
}

async function signIn(page, password) {
  await page.goto('/');
  await page.getByLabel('Password').fill(password);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
}

/** Waits for the page to be still, then saves docs/images/dashboard/<name>.png. */
async function shot(page, name, options = {}) {
  await page.evaluate(() => {
    document.getElementById('toasts')?.replaceChildren();
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
  });
  await page.mouse.move(0, 0);
  await page.waitForTimeout(400);
  const { element, ...rest } = options;
  const file = path.join(OUT, `${name}.png`);
  if (element) await element.screenshot({ path: file, ...rest });
  else await page.screenshot({ path: file, ...rest });
}

/** Outlines an element for a screenshot (removed by the next page load). */
async function highlight(locator) {
  await locator.evaluate((el) => {
    el.style.setProperty('outline', '3px solid #e8590c');
    el.style.setProperty('outline-offset', '3px');
    el.style.setProperty('border-radius', '6px');
  });
}

/** JSON API calls with the page's session (and its CSRF token). */
function apiClient(page) {
  let csrf = null;
  return async (method, url, body) => {
    if (!csrf) csrf = (await (await page.request.get('/api/v1/auth/session')).json()).csrf_token;
    const res = await page.request.fetch(`/api/v1${url}`, {
      method,
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
      data: method === 'GET' ? undefined : JSON.stringify(body ?? {}),
    });
    const text = await res.text();
    if (!res.ok()) throw new Error(`${method} ${url}: ${res.status()} ${text}`);
    return text ? JSON.parse(text) : null;
  };
}

async function waitDeployment(api, id) {
  await expect.poll(async () => (await api('GET', `/deployments/${id}`)).status, { timeout: 240_000, intervals: [1000] })
    .not.toMatch(/^(Queued|InProgress)$/);
  return api('GET', `/deployments/${id}`);
}

function commit(repo, gitEnv, version, message) {
  const index = path.join(repo, 'index.js');
  fs.writeFileSync(index, fs.readFileSync(index, 'utf8').replace(/version: '[^']*'/, `version: '${version}'`));
  execFileSync('git', ['commit', '-qam', message], { cwd: repo, env: { ...process.env, ...gitEnv } });
}

/** A request through the built-in proxy, as a visitor of the domain would send it. */
function visit(port, host, urlPath) {
  return new Promise((resolve) => {
    const req = http.request({ host: '127.0.0.1', port, path: urlPath, headers: { Host: host } }, (res) => {
      res.resume();
      res.on('end', () => resolve(res.statusCode));
    });
    req.on('error', () => resolve(null));
    req.end();
  });
}

test('dashboard guide screenshots', async ({ browser }) => {
  const e = env();
  fs.mkdirSync(OUT, { recursive: true });
  const python = process.platform === 'win32' ? 'python' : 'python3';

  // --- Sign-in page ------------------------------------------------------
  const context = await browser.newContext();
  await prepare(context);
  const page = await context.newPage();
  await page.goto('/');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
  await shot(page, 'sign-in');
  await signIn(page, e.password);
  const api = apiClient(page);

  // --- Update notice (from the fake release API) ----------------------------
  const banner = page.locator('[data-update="available"]');
  await expect(banner).toBeVisible();
  await shot(page, 'update-banner', { clip: { x: 0, y: 0, width: 1280, height: 150 } });
  await banner.locator('[data-help="update"]').click();
  await expect(page.locator('[data-help-pop="update"]')).toBeVisible();
  await shot(page, 'update-help', { clip: { x: 0, y: 0, width: 1280, height: 560 } });
  await page.keyboard.press('Escape');
  await banner.getByRole('button', { name: 'Later' }).click();

  // --- New app wizard: "shop" from a git repository -------------------------
  await page.getByRole('link', { name: '+ New app' }).click();
  await page.getByLabel('Repository URL').fill(e.repo);
  await page.getByLabel('App name', { exact: true }).fill('shop');
  await shot(page, 'new-app-source');
  await page.getByRole('button', { name: 'Detect settings →' }).click();
  await expect(page.getByText('Detected: Node.js')).toBeVisible();
  await page.getByLabel('Port').fill(String(SHOP_PORT));
  await page.getByLabel('Test command').fill('echo all tests passed');
  await shot(page, 'new-app-review');
  await page.getByRole('button', { name: 'Create app' }).click();
  await expect(page).toHaveURL(/#\/apps\/shop$/);
  // The first deployment, caught while it runs if we can.
  const running = page.locator('[data-deployment] .stage.running');
  if (await running.first().waitFor({ timeout: 20_000 }).then(() => true, () => false)) {
    await shot(page, 'deploy-running');
  }
  await expect(page.locator('[data-deployment] [data-status="Success"]')).toBeVisible({ timeout: 240_000 });
  const shop = await api('GET', '/projects/shop');

  // --- Users, tokens --------------------------------------------------------
  await api('POST', '/users', { username: 'alice', password: 'demo-password-alice-1', role: 'deployer' });
  await api('POST', '/users', { username: 'bob', password: 'demo-password-bob-123', role: 'viewer' });
  await api('POST', '/tokens', { name: 'ci-deploy', scope: 'deploy' });
  await api('POST', '/tokens', { name: 'monitoring', scope: 'read' });

  // --- Settings: domain, two instances, variables, secret, schedule, webhook
  let proxyOk = false;
  try {
    await api('POST', `/projects/${shop.id}/domains`, { domain: 'shop.demo.test', tls: 'off' });
    proxyOk = true;
  } catch (err) {
    console.log(`domain not added: ${err.message}`);
  }
  await api('PUT', `/projects/${shop.id}/settings`, {
    port: SHOP_PORT,
    test_command: 'echo all tests passed',
    env: { NODE_ENV: 'production', GREETING: 'hello' },
    ...(proxyOk ? { instances: 2 } : {}),
  });
  await api('PUT', `/projects/${shop.id}/secrets/DATABASE_URL`, { value: 'postgres://shop:demo@127.0.0.1/shop' });
  await api('PUT', `/projects/${shop.id}/schedule`, { hour: 2, minute: 30 });
  await api('POST', `/projects/${shop.id}/webhook`);

  // A deployment that fails (a commit that doesn't exist), for the history.
  const failed = await api('POST', '/projects/shop/deploy', { commit: '0badc0de0badc0de0badc0de0badc0de0badc0d' });
  await waitDeployment(api, failed.deployment_id);

  // --- v2 with a Rolling deployment (two instances behind the proxy) ---------
  commit(e.repo, e.gitEnv, 'v2', 'Add the version to the status page');
  await page.goto('/#/apps/shop/deploy');
  await expect(page.locator(`[data-deployment="${failed.deployment_id}"]`)).toBeVisible();
  await page.getByLabel('Strategy').selectOption(proxyOk ? 'Rolling' : 'GracefulSwitch');
  await page.locator('form').getByRole('button', { name: 'Deploy' }).click();
  await expect(page.locator(`[data-deployment="${failed.deployment_id}"]`)).toHaveCount(0);
  const latest = page.locator('[data-deployment]');
  await expect(latest.locator('[data-status="Success"], [data-status="Failed"], [data-status="Rolled back"]').first())
    .toBeVisible({ timeout: 240_000 });

  // Some traffic for the logs.
  if (proxyOk) {
    for (let i = 0; i < 4; i += 1) {
      for (const p of ['/', '/products', '/cart', '/slow', '/checkout', '/products/42']) {
        await visit(e.proxyHttp, 'shop.demo.test', p);
      }
    }
  }

  // Limits, where this server supports them.
  const caps = await api('GET', '/resources');
  if (caps.cpu || caps.memory) {
    await api('PUT', `/projects/${shop.id}/resources`, {
      cpu_cores: caps.cpu ? 0.5 : null,
      memory_bytes: caps.memory ? 256 * MIB : null,
      pids_max: null,
      force: true,
    }).catch((err) => console.log(`limits not set: ${err.message}`));
  }

  // --- "worker": a directory on the server, no HTTP port ----------------------
  const detected = await api('POST', '/detect', { source_dir: e.worker });
  const worker = await api('POST', '/projects', { name: 'worker', source_dir: e.worker, runtime: detected.runtime });
  await api('PUT', `/projects/${worker.id}/settings`, { start_command: `${python} -u app.py`, health_check_url: '' });
  const wd = await api('POST', '/projects/worker/deploy', {});
  await waitDeployment(api, wd.deployment_id);

  // --- "web": a Docker container app ----------------------------------------
  if (e.web) {
    const d = await api('POST', '/detect', { source_dir: e.web });
    await api('POST', '/projects', { name: 'web', source_dir: e.web, runtime: d.runtime });
    const dep = await api('POST', '/projects/web/deploy', {});
    const done = await waitDeployment(api, dep.deployment_id);
    console.log(`docker app: ${done.status}`);
  }

  await api('POST', '/backups', { include_releases: false });

  // --- Overview -----------------------------------------------------------
  await page.goto('/#/');
  await expect(page.locator('[data-app="shop"] [data-status="Running"]')).toBeVisible();
  await expect(page.locator('[data-tile="cpu"] .value')).not.toBeEmpty();
  await page.waitForTimeout(6000); // CPU and memory of the apps
  await page.reload();
  await expect(page.locator('[data-app="worker"]')).toBeVisible();
  await shot(page, 'overview');
  await page.locator('[data-help="overview.apps"]').click();
  await expect(page.locator('[data-help-pop="overview.apps"]')).toBeVisible();
  await shot(page, 'help-popover');
  await page.keyboard.press('Escape');

  // --- App page tabs ----------------------------------------------------------
  await page.goto('/#/apps/shop/deploy');
  await expect(page.locator('[data-deployment]')).toBeVisible();
  await expect(page.locator('[data-deployment] .stage').first()).toBeVisible();
  await shot(page, 'app-deploy', { fullPage: true });
  if (await page.locator('[data-instances]').count()) {
    await shot(page, 'app-instances', { element: page.locator('[data-instances]') });
  }

  await page.goto('/#/apps/shop/releases');
  await expect(page.locator('[data-release]').first()).toBeVisible();
  await shot(page, 'app-releases');

  await page.goto('/#/apps/shop/logs');
  await expect(page.locator('[data-log-status]')).toHaveText('Live');
  await expect(page.locator('[data-log] .line').first()).toBeVisible();
  await shot(page, 'app-logs');

  await page.goto('/#/apps/shop/resources');
  await expect(page.locator('[data-stat="memory"] .value')).not.toHaveText('–');
  await page.waitForTimeout(25_000); // let the live charts fill in
  await shot(page, 'app-resources', { fullPage: true });

  await page.goto('/#/apps/shop/settings');
  await expect(page.locator('#domains')).toBeVisible();
  await expect(page.locator('ul.deliveries, .card p.muted.small').first()).toBeVisible();
  await page.waitForTimeout(1000);
  await shot(page, 'app-settings', { fullPage: true });
  await shot(page, 'app-domains', { element: page.locator('#domains') });

  await page.goto('/#/apps/shop/activity');
  await expect(page.locator('[data-events] li').first()).toBeVisible();
  await shot(page, 'app-activity');

  if (e.web) {
    await page.goto('/#/apps/web/deploy');
    await expect(page.locator('[data-runtime="docker"]')).toBeVisible();
    await shot(page, 'docker-app');
  }

  // --- Deployments, events --------------------------------------------------
  await page.goto('/#/deployments');
  await expect(page.locator('tr[data-deployment]').first()).toBeVisible();
  await shot(page, 'deployments');
  await page.goto(`/#/deployments/${failed.deployment_id}`);
  await expect(page.locator('[data-error]')).toBeVisible();
  await shot(page, 'deployment-failed');

  await page.goto('/#/events');
  await expect(page.locator('[data-events] li').first()).toBeVisible();
  await shot(page, 'events');

  // --- Account --------------------------------------------------------------
  await page.goto('/#/account');
  await expect(page.locator('[data-user="alice"]')).toBeVisible();
  await expect(page.locator('[data-backup]').first()).toBeVisible();
  await shot(page, 'account', { fullPage: true });

  // --- Help and shortcuts, dark theme ----------------------------------------
  await page.goto('/#/');
  await expect(page.locator('[data-app="shop"]')).toBeVisible();
  await page.keyboard.press('?');
  await expect(page.locator('dialog[open]')).toBeVisible();
  await shot(page, 'shortcuts');
  await page.keyboard.press('Escape');
  await page.keyboard.press('t');
  await page.goto('/#/apps/shop/deploy');
  await expect(page.locator('[data-deployment] .stage').first()).toBeVisible();
  await shot(page, 'dark-app');
  await page.keyboard.press('t');

  // --- The first-run tour (a context where it hasn't been seen) -------------
  const tourContext = await browser.newContext();
  await prepare(tourContext, { dismissUpdate: true, tourDone: false });
  const tp = await tourContext.newPage();
  await signIn(tp, e.password);
  const tourCard = tp.locator('.tour-card');
  await expect(tourCard).toContainText('Welcome to Aegis');
  await shot(tp, 'tour-welcome');
  for (const id of ['deploy', 'tabs']) {
    while ((await tp.locator('[data-tour-overlay]').getAttribute('data-step')) !== id) {
      await tourCard.locator('[data-tour-action="next"]').click();
      await tp.waitForTimeout(400);
    }
    await tp.waitForTimeout(600);
    await shot(tp, `tour-${id}`);
  }
  await tp.keyboard.press('Escape');
  await tourContext.close();

  // --- Phone ------------------------------------------------------------------
  const phone = await browser.newContext({
    ...devices['Pixel 7'], deviceScaleFactor: 2, colorScheme: 'light', reducedMotion: 'reduce', timezoneId: 'UTC', locale: 'en-US',
  });
  await prepare(phone, { dismissUpdate: true });
  const mp = await phone.newPage();
  await signIn(mp, e.password);
  await expect(mp.locator('[data-app="shop"]')).toBeVisible();
  await shot(mp, 'mobile-overview');
  await mp.goto('/#/apps/shop/deploy');
  await expect(mp.locator('[data-deployment] .stage').first()).toBeVisible();
  await mp.locator('[data-help="app.deploy"]').click();
  await expect(mp.locator('[data-help-pop="app.deploy"]')).toBeVisible();
  await shot(mp, 'mobile-help');
  await phone.close();

  await context.close();
});
