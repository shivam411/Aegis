// The Phase 0 scenario, done entirely in the browser: create an app from a
// git repository, deploy it, watch the pipeline, read logs, stop/start/
// restart, redeploy, roll back, and audit what happened.
const { test, expect, env, login, confirm, appBody, commit } = require('./fixtures');
const fs = require('fs');

const APP_PORT = Number(process.env.AEGIS_E2E_APP_PORT || 18650);

test.describe.configure({ mode: 'serial' });

test('sign-in is required and wrong passwords are rejected', async ({ page }) => {
  await page.goto('/');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
  await page.getByLabel('Password').fill('not-the-password');
  await page.getByRole('button', { name: 'Sign in' }).click();
  await expect(page.getByRole('alert')).toContainText(/invalid|incorrect|wrong/i);
  await expect(page.getByRole('heading', { name: 'Overview' })).toHaveCount(0);

  // The API itself refuses the browser without a session.
  const res = await page.request.get('/api/v1/projects');
  expect(res.status()).toBe(401);
});

test('overview shows host resources and an empty state', async ({ page }) => {
  await login(page);
  for (const tile of ['cpu', 'memory', 'disk', 'load']) {
    await expect(page.locator(`[data-tile="${tile}"] .value`)).not.toBeEmpty();
  }
  await expect(page.locator('[data-tile="memory"] [role="meter"]')).toHaveAttribute('aria-valuenow', /\d+/);
  await expect(page.getByText('No apps yet')).toBeVisible();
  await expect(page.locator('#conn')).toHaveText('Live');
});

test('full lifecycle from the browser', async ({ page }) => {
  const { repo } = env();
  await login(page);

  await test.step('new app wizard detects and saves settings', async () => {
    await page.getByRole('link', { name: '+ New app' }).click();
    await page.getByLabel('Repository URL').fill(repo);
    await page.getByLabel('Branch').fill('main');
    await page.getByLabel('App name', { exact: true }).fill('shop');
    await page.getByRole('button', { name: 'Detect settings →' }).click();
    await expect(page.getByText('Detected: Node.js')).toBeVisible();
    await expect(page.getByLabel('Start command')).toHaveValue('npm start');
    await expect(page.getByLabel('Health check URL')).toHaveValue('http://127.0.0.1:3000/health');
    // Changing the port moves the health check with it.
    await page.getByLabel('Port').fill(String(APP_PORT));
    await expect(page.getByLabel('Health check URL')).toHaveValue(`http://127.0.0.1:${APP_PORT}/health`);
    await page.getByLabel('Test command').fill('node -e "process.exit(0)"');
    await page.getByRole('button', { name: 'Create app' }).click();
    await expect(page).toHaveURL(/#\/apps\/shop$/);
  });

  await test.step('the first deployment runs through all seven stages', async () => {
    const pipeline = page.locator('[data-deployment]');
    for (const stage of ['Clone', 'Install', 'Build', 'Test', 'Package', 'Verify', 'Promote']) {
      await expect(pipeline.locator(`[data-stage="${stage}"]`)).toHaveAttribute('data-state', /success|skipped/, { timeout: 90_000 });
    }
    await expect(pipeline.locator('[data-status="Success"]')).toBeVisible();
    await expect(pipeline.getByText('is live')).toBeVisible();
    await expect(page.locator('.page-header [data-status="Running"]')).toBeVisible();
    expect((await appBody(APP_PORT)).version).toBe('v1');
    // The dashboard's settings reached the build.
    await pipeline.getByText('Build log').click();
    await expect(pipeline.locator('[data-build-log]')).toContainText('Applied project settings');
  });

  await test.step('overview card', async () => {
    await page.getByRole('link', { name: 'Overview' }).click();
    const card = page.locator('[data-app="shop"]');
    await expect(card.locator('[data-status="Running"]')).toBeVisible();
    await expect(card.locator('.facts')).toContainText('Restarts0');
    await card.getByRole('link', { name: 'shop' }).click();
  });

  await test.step('live logs: search, pause, download', async () => {
    await page.getByRole('link', { name: 'Logs' }).click();
    const log = page.locator('[data-log]');
    await expect(page.locator('[data-log-status]')).toHaveText('Live');
    await expect(log).toContainText(`running on port ${APP_PORT}`);
    await page.getByLabel('Search logs').fill('running on port');
    await expect(log.locator('mark').first()).toHaveText('running on port');
    await page.getByLabel('Search logs').fill('no-such-line');
    await expect(log).toContainText('No lines match');
    await page.getByLabel('Search logs').fill('');
    await page.getByRole('button', { name: 'Pause' }).click();
    await expect(page.getByRole('button', { name: 'Resume' })).toBeVisible();
    await page.getByRole('button', { name: 'Resume' }).click();
    const [download] = await Promise.all([
      page.waitForEvent('download'),
      page.getByRole('button', { name: 'Download' }).click(),
    ]);
    expect(download.suggestedFilename()).toMatch(/^shop-.*\.log$/);
    const text = fs.readFileSync(await download.path(), 'utf8');
    expect(text).toContain(`running on port ${APP_PORT}`);
  });

  await test.step('restart, stop and start with confirmations', async () => {
    const header = page.locator('.page-header');
    await header.getByRole('button', { name: 'Restart' }).click();
    await confirm(page, 'Restart');
    await expect(header).toContainText('1 restart');
    await expect(header.locator('[data-status="Running"]')).toBeVisible();

    // Cancelling does nothing.
    await header.getByRole('button', { name: 'Stop' }).click();
    await confirm(page, 'Cancel');
    await expect(header.locator('[data-status="Running"]')).toBeVisible();

    await header.getByRole('button', { name: 'Stop' }).click();
    await confirm(page, 'Stop');
    await expect(header.locator('[data-status="Stopped"]')).toBeVisible();
    await expect.poll(() => appBody(APP_PORT)).toBeNull();

    await header.getByRole('button', { name: 'Start' }).click();
    await expect(header.locator('[data-status="Running"]')).toBeVisible();
    await expect.poll(async () => (await appBody(APP_PORT))?.version).toBe('v1');
  });

  await test.step('redeploy a new commit', async () => {
    commit(repo, 'v2');
    await page.getByRole('link', { name: 'Deploy', exact: true }).click();
    await page.getByLabel('Strategy').selectOption('GracefulSwitch');
    await page.locator('form').getByRole('button', { name: 'Deploy' }).click();
    const pipeline = page.locator('[data-deployment]');
    await expect(pipeline.locator('[data-status="Success"]')).toBeVisible({ timeout: 90_000 });
    await expect.poll(async () => (await appBody(APP_PORT))?.version).toBe('v2');
  });

  await test.step('roll back from the release history', async () => {
    await page.getByRole('link', { name: 'Releases' }).click();
    const rows = page.locator('[data-release]');
    await expect(rows).toHaveCount(2);
    await expect(rows.first().locator('[data-status="Live"]')).toBeVisible();
    await rows.nth(1).getByRole('button', { name: 'Roll back' }).click();
    await confirm(page, 'Roll back');
    await expect(rows.nth(1).locator('[data-status="Live"]')).toBeVisible();
    await expect.poll(async () => (await appBody(APP_PORT))?.version).toBe('v1');
  });

  await test.step('settings are saved as dashboard overrides', async () => {
    await page.getByRole('link', { name: 'Settings' }).click();
    await expect(page.getByLabel('Port')).toHaveValue(String(APP_PORT));
    await page.getByLabel('Build command').fill('echo dashboard build');
    await page.getByRole('button', { name: 'Save settings' }).click();
    await expect(page.locator('.toast', { hasText: 'Settings saved' })).toBeVisible();
    // The server validates what the browser can't.
    await page.getByLabel('Health check URL').fill('ftp://example.com/');
    await page.getByRole('button', { name: 'Save settings' }).click();
    await expect(page.locator('.toast.bad', { hasText: 'health check URL' })).toBeVisible();
    await page.reload();
    await expect(page.getByLabel('Build command')).toHaveValue('echo dashboard build');
    await page.getByRole('button', { name: 'Generate webhook secret' }).click();
    await confirm(page, 'Generate');
    await expect(page.locator('[data-secret]').nth(1)).toHaveText(/\S{20,}/);
  });

  await test.step('deployments timeline and stage detail', async () => {
    await page.getByRole('link', { name: 'Deployments' }).click();
    const rows = page.locator('tr[data-deployment]');
    await expect(rows).toHaveCount(2);
    await expect(rows.first()).toContainText('shop');
    await page.getByLabel('Status').selectOption('Failed');
    await expect(page.getByText('No deployments match.')).toBeVisible();
    await page.getByLabel('Status').selectOption('');
    await rows.last().click();
    await expect(page).toHaveURL(/#\/deployments\/.+/);
    await expect(page.locator('[data-stage="Promote"]')).toHaveAttribute('data-state', 'success');
    const events = page.locator('[data-events] li');
    await expect(events.first()).toHaveAttribute('data-event-type', 'DeploymentQueued');
    await expect(page.locator('[data-events]')).toContainText('BuildStageTest');
  });

  await test.step('audit log with filters', async () => {
    await page.getByRole('link', { name: 'Events' }).click();
    await page.getByLabel('Category').selectOption('process');
    const list = page.locator('[data-events]');
    await expect(list.locator('[data-event-type="ProcessStopRequested"]')).toContainText('by user:admin');
    await expect(list.locator('[data-event-type="UserLoggedIn"]')).toHaveCount(0);
    await page.getByLabel('Category').selectOption('access');
    await expect(list.locator('[data-event-type="UserLoginFailed"]').first()).toBeVisible();
    await page.getByLabel('Category').selectOption('all');
    await page.getByLabel('Filter events').fill('RollbackCompleted');
    await expect(list.locator('li')).toHaveCount(1);
  });
});

test('API tokens, theme, shortcuts and sign-out', async ({ page }) => {
  await login(page);
  await page.getByRole('link', { name: 'Account' }).click();
  await page.getByLabel('Name').fill('ci');
  await page.getByLabel('Scope').selectOption('read');
  await page.getByRole('button', { name: 'Create token' }).click();
  const token = await page.locator('[data-secret]').textContent();
  expect(token).toMatch(/^aegis_/);
  // The new token works against the API.
  const res = await page.request.get('/api/v1/projects', { headers: { Authorization: `Bearer ${token}` } });
  expect(res.status()).toBe(200);

  await page.locator('[data-token="ci"]').getByRole('button', { name: 'Revoke' }).click();
  await confirm(page, 'Revoke');
  await expect(page.locator('[data-token="ci"]')).toHaveCount(0);
  const revoked = await page.request.get('/api/v1/projects', { headers: { Authorization: `Bearer ${token}` } });
  expect(revoked.status()).toBe(401);

  await page.getByLabel('Theme', { exact: true }).selectOption('dark');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await page.keyboard.press('t');
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');

  await page.keyboard.press('g');
  await page.keyboard.press('d');
  await expect(page).toHaveURL(/#\/deployments$/);
  await page.keyboard.press('?');
  await expect(page.locator('dialog[open]')).toContainText('Keyboard shortcuts');
  await page.keyboard.press('Escape');
  await expect(page.locator('dialog[open]')).toHaveCount(0);

  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
  const after = await page.request.get('/api/v1/projects');
  expect(after.status()).toBe(401);
});
