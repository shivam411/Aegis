// An app registered with the wrong source: raise its health-check and drain
// timeouts in Settings → Build & run, switch it to a folder on the server
// (Settings → Repository → Change source), then delete it (Danger zone).
const { test, expect, env, login, confirm } = require('./fixtures');

test.describe.configure({ mode: 'serial' });

const NAME = 'e2e-wrong-source';

/** Calls the API from the signed-in page. */
async function apiCall(page, method, path, body) {
  return page.evaluate(async ({ method, path, body }) => {
    const { csrf_token: csrf } = await (await fetch('/api/v1/auth/session')).json();
    const res = await fetch(`/api/v1${path}`, {
      method,
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    return { status: res.status, text: await res.text() };
  }, { method, path, body });
}

async function project(page) {
  const res = await apiCall(page, 'GET', `/projects/${NAME}`);
  return res.status === 200 ? JSON.parse(res.text) : null;
}

test('timeouts, change of source and deleting an app', async ({ page }) => {
  const { envApp } = env();
  await login(page);
  const created = await apiCall(page, 'POST', '/projects', {
    name: NAME,
    repository_url: 'https://example.invalid/acme/wrong.git',
    branch: 'main',
  });
  expect(created.status, created.text).toBe(201);

  await test.step('health check and drain timeouts', async () => {
    await page.goto(`/#/apps/${NAME}/settings`);
    const health = page.getByLabel('Health check timeout (seconds)');
    const drain = page.getByLabel('Drain timeout (seconds)');
    await expect(health).toHaveValue('');
    await expect(health).toHaveAttribute('placeholder', '30');
    await health.fill('75');
    await drain.fill('5');
    await page.getByRole('button', { name: 'Save settings' }).click();
    const diff = page.locator('dialog[open] pre.diff');
    await expect(diff.locator('.add', { hasText: 'health_check_timeout_secs = 75' })).toBeVisible();
    await expect(diff.locator('.add', { hasText: 'drain_timeout_secs = 5' })).toBeVisible();
    await confirm(page, 'Save settings');
    await expect(page.locator('.toast', { hasText: 'Settings saved' })).toBeVisible();
    let p = await project(page);
    expect(p.settings.health_check_timeout_secs).toBe(75);
    expect(p.settings.drain_timeout_secs).toBe(5);

    // Out of range: refused, nothing saved.
    await page.reload();
    await expect(page.getByLabel('Health check timeout (seconds)')).toHaveValue('75');
    await page.getByLabel('Health check timeout (seconds)').fill('4000');
    await page.getByRole('button', { name: 'Save settings' }).click();
    await expect(page.locator('.toast.bad', { hasText: 'Health check timeout' })).toBeVisible();
    p = await project(page);
    expect(p.settings.health_check_timeout_secs).toBe(75);

    // Saving another field keeps the timeouts (the form sends every field).
    await page.reload();
    await page.getByLabel('Start command').fill('node index.js');
    await page.getByRole('button', { name: 'Save settings' }).click();
    await confirm(page, 'Save settings');
    await expect(page.locator('.toast', { hasText: 'Settings saved' })).toBeVisible();
    p = await project(page);
    expect(p.settings.start_command).toBe('node index.js');
    expect(p.settings.health_check_timeout_secs).toBe(75);
    expect(p.settings.drain_timeout_secs).toBe(5);
  });

  await test.step('change the source to a folder on the server', async () => {
    await page.goto(`/#/apps/${NAME}/settings`);
    const card = page.locator('[data-card="repository"]');
    await expect(card.locator('[data-source]')).toContainText('wrong.git');
    await card.getByRole('button', { name: 'Change source' }).click();
    await card.getByLabel('Folder on this server').check();
    const folder = card.locator('#source-dir');
    // A folder that doesn't exist is refused like at registration.
    await folder.fill(`${envApp}-missing`);
    await card.getByRole('button', { name: 'Save source' }).click();
    await confirm(page, 'Change source');
    await expect(page.locator('.toast.bad', { hasText: 'Source directory' })).toBeVisible();
    await folder.fill(envApp);
    await card.getByRole('button', { name: 'Save source' }).click();
    await confirm(page, 'Change source');
    await expect(page.locator('.toast', { hasText: 'Source changed' })).toBeVisible();
    await expect(page.locator('[data-card="repository"] [data-source]')).toContainText('envapp');
    const p = await project(page);
    expect(p.repository_url).toBe('');
    expect(p.source_dir).toContain('envapp');
    // Settings are untouched.
    expect(p.settings.health_check_timeout_secs).toBe(75);
    const events = JSON.parse((await apiCall(page, 'GET', `/events?project=${NAME}&limit=50`)).text);
    expect(events.map((e) => e.event_type)).toContain('ProjectSourceChanged');
  });

  await test.step('delete the app', async () => {
    await page.goto(`/#/apps/${NAME}/settings`);
    const zone = page.locator('[data-card="danger-zone"]');
    await zone.getByRole('button', { name: 'Delete app' }).click();
    const dialog = page.locator('dialog[open]');
    const go = dialog.getByRole('button', { name: 'Delete this app' });
    await expect(go).toBeDisabled();
    await dialog.getByLabel('App name').fill('not-the-name');
    await expect(go).toBeDisabled();
    await dialog.getByLabel('App name').fill(NAME);
    await expect(go).toBeEnabled();
    await go.click();
    await expect(dialog).toHaveCount(0);
    await expect(page.locator('.toast', { hasText: `Deleted ${NAME}` })).toBeVisible();
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
    expect(await project(page)).toBeNull();
    // The history stays; the name is free again.
    const events = JSON.parse((await apiCall(page, 'GET', '/events?limit=1000')).text);
    expect(events.map((e) => e.event_type)).toContain('ProjectDeleted');
    const again = await apiCall(page, 'POST', '/projects', { name: NAME, source_dir: envApp });
    expect(again.status, again.text).toBe(201);
    const id = JSON.parse(again.text).id;
    const gone = await apiCall(page, 'DELETE', `/projects/${id}?confirm=${NAME}`);
    expect(gone.status, gone.text).toBe(200);
  });
});
